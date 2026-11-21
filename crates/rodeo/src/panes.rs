use std::collections::HashMap;
use std::time::{Duration, Instant};

use vt::Terminal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Idle,
    Attaching,
    Attached,
    Lost(String),
    Ended(String),
    Failed(String),
}

impl Link {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Attaching | Self::Attached)
    }
}

const QUEUE_LIMIT: usize = 1 << 20;
const SYNC_TIMEOUT: Duration = Duration::from_secs(1);
const ORACLE_RETRY: Duration = Duration::from_secs(5);

pub struct Pane {
    pub address: String,
    queued: Vec<u8>,
    pub term: Terminal,
    pub writer: Option<hc::AttachWriter>,
    pub link: Link,
    pub attach: u64,
    /// A second attach on the same terminal that only reports its size.
    pub oracle: Option<hc::AttachWriter>,
    pub oracle_attach: u64,
    oracle_pending: bool,
    oracle_retry: Option<Instant>,
    pub size: (u16, u16),
    pub shown: bool,
    pub led: Option<(u16, u16)>,
    pub focus_sent: Option<bool>,
    pub notifications: Vec<(String, String)>,
    pub title: String,
    synced: Option<Instant>,
}

impl Pane {
    pub fn new(address: String, size: (u16, u16), options: vt::Options) -> Result<Self, vt::Error> {
        Ok(Self {
            address,
            queued: Vec::new(),
            term: Terminal::new(size.0.max(1), size.1.max(1), options)?,
            writer: None,
            link: Link::Idle,
            oracle: None,
            oracle_attach: 0,
            oracle_pending: false,
            oracle_retry: None,
            attach: 0,
            size,
            shown: false,
            led: None,
            focus_sent: None,
            notifications: Vec::new(),
            title: String::new(),
            synced: None,
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.feed(bytes);
        self.hold(Instant::now());
        self.reply();
    }

    pub fn hold(&mut self, now: Instant) -> Option<Instant> {
        if !self.term.synchronized_output() {
            self.synced = None;
            return None;
        }
        let until = *self.synced.get_or_insert(now) + SYNC_TIMEOUT;
        if now < until {
            return Some(until);
        }
        self.term.end_synchronized_output();
        self.synced = None;
        None
    }

    pub fn input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match &self.writer {
            Some(writer) => {
                if let Err(error) = writer.input(bytes.to_vec()) {
                    tracing::debug!(address = %self.address, %error, "input dropped");
                }
            }
            None if matches!(self.link, Link::Idle | Link::Attaching)
                && self.queued.len() + bytes.len() <= QUEUE_LIMIT =>
            {
                self.queued.extend_from_slice(bytes);
            }
            None => tracing::debug!(address = %self.address, "input dropped: not attached"),
        }
    }

    pub fn queued(&self) -> &[u8] {
        &self.queued
    }

    pub fn connect(&mut self, writer: hc::AttachWriter) {
        let queued = std::mem::take(&mut self.queued);
        if !queued.is_empty()
            && let Err(error) = writer.input(queued)
        {
            tracing::debug!(address = %self.address, %error, "queued input dropped");
        }
        self.writer = Some(writer);
        self.link = Link::Attached;
    }

    pub fn end(&mut self, link: Link) {
        self.queued.clear();
        self.link = link;
    }

    pub fn reply(&mut self) {
        let replies = self.term.take_replies();
        if replies.is_empty() {
            return;
        }
        if let Some(writer) = &self.writer {
            let _ = writer.automatic_input(replies);
        }
    }

    pub fn resize(&mut self, size: (u16, u16), cell_px: (u32, u32)) {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size && self.term.cell_px() == cell_px {
            return;
        }
        self.size = size;
        if let Err(error) = self.term.resize(size.0, size.1, cell_px) {
            tracing::warn!(address = %self.address, %error, "vt resize failed");
        }
        self.reply();
    }

    /// Resize the local VT to a size hc reports without changing what we lead.
    /// The shared terminal follows its most recently active viewer, so a
    /// foreign viewer can resize it under us; the byte stream then arrives
    /// formatted for that size and a stale local VT garbles and scrolls.
    pub fn adapt(&mut self, size: (u16, u16)) {
        let size = (size.0.max(1), size.1.max(1));
        if self.term.size() == size {
            return;
        }
        // A viewer that follows the bottom of the stream should keep doing so
        // across a resize the foreign size forces; one that read scrollback
        // stays where it was put.
        let at_bottom = !self.term.is_scrolled_back();
        if let Err(error) = self.term.resize(size.0, size.1, self.term.cell_px()) {
            tracing::warn!(address = %self.address, %error, "vt adapt failed");
        }
        if at_bottom {
            self.term.follow_cursor(self.size.1);
        }
        self.reply();
    }

    /// hc told us the shared terminal is now this size. When it is not the size
    /// we lead, a foreign viewer moved it: drop the lead so the next relayout
    /// re-leads our size instead of trusting a stale `led`.
    pub fn stream_resized(&mut self, cols: u16, rows: u16) {
        let size = (cols.max(1), rows.max(1));
        if size != self.size {
            self.led = None;
        }
        self.adapt(size);
    }

    /// The size the incoming stream is formatted for while it is not the size
    /// this pane has: another viewer leads the shared terminal, so what the
    /// pane shows is a crop of a bigger screen.
    pub fn foreign_size(&self) -> Option<(u16, u16)> {
        (self.shown && self.link == Link::Attached && self.term.size() != self.size)
            .then_some(self.term.size())
    }

    /// Whether an observer attach is open or on its way.
    pub fn oracle_open(&self) -> bool {
        self.oracle.is_some() || self.oracle_pending
    }

    /// Claim an observer attach: `None` when one is already open, on its way,
    /// or waiting out the retry after a failure. relayout runs every frame, so
    /// a core that refuses the observer must not be asked again every frame.
    pub fn begin_oracle(&mut self) -> Option<u64> {
        if self.oracle_open() || self.oracle_retry.is_some_and(|at| at > Instant::now()) {
            return None;
        }
        self.oracle_pending = true;
        self.oracle_attach += 1;
        Some(self.oracle_attach)
    }

    /// Take the observer's writer. A false answer means the pane stopped
    /// wanting it while the attach was in flight, so it is closed here.
    pub fn oracle_connected(&mut self, attach: u64, writer: hc::AttachWriter) -> bool {
        self.oracle_pending = false;
        self.oracle_retry = None;
        if attach != self.oracle_attach || self.link != Link::Attached || !self.shown {
            let _ = writer.close();
            return false;
        }
        self.oracle = Some(writer);
        true
    }

    pub fn oracle_failed(&mut self, attach: u64) {
        if attach == self.oracle_attach {
            self.oracle_pending = false;
            self.oracle_retry = Some(Instant::now() + ORACLE_RETRY);
        }
    }

    /// Route one frame of the observer. The core sends its `Resize` immediately
    /// before the paint bytes that use that size, which is what makes it worth
    /// a second connection; the bytes it carries are the same paint the main
    /// attach already delivers, so they are dropped.
    pub fn oracle_output(&mut self, attach: u64, output: Option<&hc::Output>) {
        if attach != self.oracle_attach {
            return;
        }
        match output {
            Some(hc::Output::Resize { cols, rows }) => self.stream_resized(*cols, *rows),
            Some(hc::Output::Bytes(_)) => {}
            _ => self.close_oracle(),
        }
    }

    /// Drop the observer. It must never outlive the attach that shows the size
    /// it reports: an observer is not a viewer to hc, so a terminal left with
    /// only observers goes headless and its size stops meaning anything.
    pub fn close_oracle(&mut self) {
        self.oracle_pending = false;
        self.oracle_retry = None;
        self.oracle_attach += 1;
        if let Some(writer) = self.oracle.take() {
            let _ = writer.close();
        }
    }

    pub fn lead(&mut self) {
        if !self.shown || self.link != Link::Attached || self.led == Some(self.size) {
            return;
        }
        if let Some(writer) = &self.writer
            && writer.resize(self.size.0, self.size.1).is_ok()
        {
            self.led = Some(self.size);
        }
    }

    pub fn focus(&mut self, focused: bool) {
        if self.focus_sent == Some(focused) || self.link != Link::Attached {
            return;
        }
        if let Some(writer) = &self.writer
            && writer.focus(focused).is_ok()
        {
            self.focus_sent = Some(focused);
        }
    }

    pub fn detach(&mut self) {
        self.close_oracle();
        if let Some(writer) = self.writer.take() {
            let _ = writer.close();
        }
        self.led = None;
        self.focus_sent = None;
    }
}

#[derive(Default)]
pub struct Panes {
    map: HashMap<String, Pane>,
}

impl Panes {
    pub fn get(&self, address: &str) -> Option<&Pane> {
        self.map.get(address)
    }

    pub fn get_mut(&mut self, address: &str) -> Option<&mut Pane> {
        self.map.get_mut(address)
    }

    pub fn insert(&mut self, pane: Pane) {
        self.map.insert(pane.address.clone(), pane);
    }

    pub fn rename(&mut self, from: &str, to: &str) {
        if self.map.contains_key(to) {
            return;
        }
        if let Some(mut pane) = self.map.remove(from) {
            to.clone_into(&mut pane.address);
            self.map.insert(to.to_owned(), pane);
        }
    }

    pub fn remove(&mut self, address: &str) -> Option<Pane> {
        self.map.remove(address)
    }

    pub fn contains(&self, address: &str) -> bool {
        self.map.contains_key(address)
    }

    pub fn addresses(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Pane> {
        self.map.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        self.map.values_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(size: (u16, u16)) -> Pane {
        Pane::new("t1".into(), size, vt::Options::default()).unwrap()
    }

    #[test]
    fn adapt_resizes_the_vt_without_moving_the_lead() {
        let mut pane = pane((80, 24));
        pane.led = Some((80, 24));
        pane.adapt((100, 30));
        assert_eq!(pane.term.size(), (100, 30));
        assert_eq!(pane.size, (80, 24));
        assert_eq!(pane.led, Some((80, 24)));
        pane.adapt((100, 30));
        assert_eq!(pane.term.size(), (100, 30));
    }

    #[test]
    fn resize_moves_the_lead_target() {
        let mut pane = pane((80, 24));
        pane.resize((90, 20), (8, 16));
        assert_eq!(pane.size, (90, 20));
        assert_eq!(pane.term.size(), (90, 20));
    }

    #[test]
    fn stream_resized_drops_a_stale_lead() {
        let mut pane = pane((80, 24));
        pane.led = Some((80, 24));
        pane.stream_resized(120, 40);
        assert_eq!(pane.led, None);
        assert_eq!(pane.term.size(), (120, 40));
        pane.led = Some((80, 24));
        pane.stream_resized(80, 24);
        assert_eq!(pane.led, Some((80, 24)));
    }

    #[test]
    fn one_oracle_is_open_at_a_time() {
        let mut pane = pane((80, 24));
        assert!(!pane.oracle_open());
        let first = pane.begin_oracle().expect("room for an oracle");
        assert!(pane.oracle_open());
        assert_eq!(pane.begin_oracle(), None, "one oracle at a time");
        pane.oracle_failed(first + 1);
        assert!(pane.oracle_open(), "a stale failure changes nothing");
        pane.oracle_failed(first);
        assert!(!pane.oracle_open());
        assert_eq!(
            pane.begin_oracle(),
            None,
            "a refused oracle waits out the retry"
        );
        pane.close_oracle();
        assert_eq!(
            pane.begin_oracle(),
            Some(first + 2),
            "a pane shown again asks again"
        );
    }

    #[test]
    fn an_oracle_frame_routes_or_is_dropped() {
        let mut pane = pane((80, 24));
        let attach = pane.begin_oracle().expect("an oracle");
        pane.oracle_output(
            attach + 1,
            Some(&hc::Output::Resize {
                cols: 100,
                rows: 30,
            }),
        );
        assert_eq!(pane.term.size(), (80, 24), "a stale attach says nothing");
        pane.oracle_output(attach, Some(&hc::Output::Bytes("paint".into())));
        assert_eq!(
            pane.term.size(),
            (80, 24),
            "the observer's paint is dropped"
        );
        pane.oracle_output(
            attach,
            Some(&hc::Output::Resize {
                cols: 100,
                rows: 30,
            }),
        );
        assert_eq!(pane.term.size(), (100, 30), "its resize is the point");
        assert_eq!(pane.led, None);
        pane.oracle_output(attach, Some(&hc::Output::Closed(hc::Ending::default())));
        assert!(!pane.oracle_open(), "its end tears the oracle down");
        pane.oracle_output(attach, Some(&hc::Output::Resize { cols: 60, rows: 20 }));
        assert_eq!(pane.term.size(), (100, 30), "and nothing after it counts");
    }

    #[test]
    fn foreign_size_names_a_stream_that_is_not_ours() {
        let mut pane = pane((80, 24));
        pane.shown = true;
        pane.link = Link::Attached;
        assert_eq!(pane.foreign_size(), None, "our own size is not foreign");
        pane.term = vt::Terminal::new(100, 30, vt::Options::default()).unwrap();
        assert_eq!(pane.foreign_size(), Some((100, 30)));
        pane.shown = false;
        assert_eq!(pane.foreign_size(), None, "a hidden pane shows nothing");
        pane.shown = true;
        pane.link = Link::Idle;
        assert_eq!(pane.foreign_size(), None, "nothing is arriving");
    }

    #[test]
    fn adapt_leaves_a_scrolled_back_viewer_alone() {
        let mut pane = pane((40, 20));
        for i in 0..200 {
            pane.feed(format!("row {i}\r\n").as_bytes());
        }
        pane.term.scroll(-30);
        assert!(pane.term.is_scrolled_back());
        pane.adapt((40, 25));
        assert!(
            pane.term.is_scrolled_back(),
            "reading scrollback survives a foreign size"
        );
        pane.term.scroll_to_bottom();
        pane.adapt((40, 10));
        assert!(!pane.term.is_scrolled_back());
        assert!(
            u64::from(pane.term.cursor().row) < 10,
            "the cursor is in the crop"
        );
    }

    #[test]
    fn detach_closes_the_oracle() {
        let mut pane = pane((80, 24));
        pane.begin_oracle();
        pane.detach();
        assert!(!pane.oracle_open());
    }
}
