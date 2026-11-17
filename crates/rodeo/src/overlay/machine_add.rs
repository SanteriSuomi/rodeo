use config::Palette;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use super::frame::{button, contrast, panel, popup, row, text};
use super::machines::MachineOp;
use super::rename::Answer;
use super::{Drawn, TextField};

const LABELS: [&str; 4] = [
    " host",
    " user (optional)",
    " port (optional)",
    " name (optional)",
];
const HOST: usize = 0;
const USER: usize = 1;
const PORT: usize = 2;
const NAME: usize = 3;

pub(super) struct AddForm {
    fields: [TextField; 4],
    focus: usize,
    error: Option<String>,
    inputs: [Rect; 4],
    buttons: [Rect; 3],
}

impl AddForm {
    pub(super) fn new() -> Self {
        Self {
            fields: Default::default(),
            focus: HOST,
            error: None,
            inputs: [Rect::default(); 4],
            buttons: [Rect::default(); 3],
        }
    }

    pub(super) fn submit(&mut self) -> Option<MachineOp> {
        let target = match self.target() {
            Ok(target) => target,
            Err((field, error)) => {
                self.error = Some(error.to_owned());
                self.focus = field;
                return None;
            }
        };
        let name = self.value(NAME);
        Some(MachineOp::Add {
            target,
            name: (!name.is_empty()).then_some(name),
        })
    }

    fn value(&self, field: usize) -> String {
        self.fields[field].text().trim().to_owned()
    }

    fn target(&self) -> Result<String, (usize, &'static str)> {
        let (host, user, port) = (self.value(HOST), self.value(USER), self.value(PORT));
        if host.is_empty() {
            return Err((HOST, "host is required"));
        }
        if !user.is_empty() && host.contains('@') {
            return Err((USER, "the host already names a user"));
        }
        if user.contains(['@', ' ']) {
            return Err((USER, "user cannot have @ or spaces"));
        }
        if !port.is_empty() && host.contains(':') {
            return Err((PORT, "the host already names a port"));
        }
        if !port.is_empty() && !matches!(port.parse::<u16>(), Ok(1..)) {
            return Err((PORT, "port must be a number from 1 to 65535"));
        }
        let user = if user.is_empty() {
            String::new()
        } else {
            format!("{user}@")
        };
        let port = if port.is_empty() {
            String::new()
        } else {
            format!(":{port}")
        };
        Ok(format!("{user}{host}{port}"))
    }

    fn step(&mut self, forward: bool) {
        let count = self.fields.len();
        self.focus = (self.focus + if forward { 1 } else { count - 1 }) % count;
    }

    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer, p: &Palette) -> Option<Drawn> {
        let outer = popup(area, 56, 15)?;
        let inner = panel(buf, outer, p.accent, p.panel_bg)?;
        let bold = Style::new()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD);
        let dim = Style::new().fg(p.overlay0).bg(p.panel_bg);
        text(buf, inner.x, inner.y, inner.width, "add machine", bold);
        let input = Style::new().fg(p.text).bg(p.surface0);
        let mut cursor = None;
        for (index, label) in LABELS.into_iter().enumerate() {
            let y = inner.y + 2 + 2 * u16::try_from(index).unwrap_or(0);
            text(buf, inner.x, y, inner.width, label, dim);
            let rect = Rect::new(inner.x, y + 1, inner.width, 1);
            buf.set_style(rect, input);
            let at = self.fields[index].render(
                Rect::new(rect.x + 1, rect.y, rect.width.saturating_sub(1), 1),
                buf,
                input,
            );
            if index == self.focus {
                cursor = at;
            }
            self.inputs[index] = rect;
        }
        if let Some(error) = &self.error {
            text(
                buf,
                inner.x,
                inner.y + 10,
                inner.width,
                &format!(" {error}"),
                Style::new().fg(p.red).bg(p.panel_bg),
            );
        }
        let rects = row(inner, &[8, 10, 12], 2, 11);
        let plain = Style::new()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD);
        let primary = Style::new()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD);
        button(buf, rects[0], " ↵ add ", primary);
        button(buf, rects[1], " tab next ", plain);
        button(buf, rects[2], " esc cancel ", plain);
        self.buttons = [rects[0], rects[1], rects[2]];
        Some(Drawn::new(outer, cursor))
    }

    pub(super) fn paste(&mut self, pasted: &str) {
        self.fields[self.focus].insert(pasted);
        self.error = None;
    }

    pub(super) fn key(&mut self, key: &KeyEvent) -> Answer {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Answer::Yes,
            KeyCode::Esc => return Answer::No,
            KeyCode::Tab | KeyCode::Down => self.step(true),
            KeyCode::BackTab | KeyCode::Up => self.step(false),
            KeyCode::Char('c') if ctrl => self.fields[self.focus].clear(),
            _ => {
                self.fields[self.focus].key(key);
                self.error = None;
            }
        }
        Answer::Stay
    }

    pub(super) fn mouse(&mut self, at: Position) -> Answer {
        if self.buttons[0].contains(at) {
            Answer::Yes
        } else if self.buttons[1].contains(at) {
            self.step(true);
            Answer::Stay
        } else if let Some(index) = self.inputs.iter().position(|r| r.contains(at)) {
            self.focus = index;
            Answer::Stay
        } else {
            Answer::No
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(values: [&str; 4]) -> AddForm {
        let mut form = AddForm::new();
        for (field, value) in form.fields.iter_mut().zip(values) {
            field.insert(value);
        }
        form
    }

    fn add(target: &str, name: Option<&str>) -> MachineOp {
        MachineOp::Add {
            target: target.into(),
            name: name.map(str::to_owned),
        }
    }

    #[test]
    fn the_add_form_needs_a_host() {
        let mut form = AddForm::new();
        assert_eq!(form.submit(), None);
        assert_eq!(form.error.as_deref(), Some("host is required"));
        assert_eq!(
            filled([" vps ", "", "", ""]).submit(),
            Some(add("vps", None))
        );
        assert_eq!(
            filled(["me@vps", "", "", " vps "]).submit(),
            Some(add("me@vps", Some("vps")))
        );
    }

    #[test]
    fn user_and_port_join_the_host() {
        assert_eq!(
            filled(["box.tail", "can", "", ""]).submit(),
            Some(add("can@box.tail", None))
        );
        assert_eq!(
            filled(["10.0.0.7", "", "2222", ""]).submit(),
            Some(add("10.0.0.7:2222", None))
        );
        assert_eq!(
            filled(["10.0.0.7", " can ", " 2222 ", "box"]).submit(),
            Some(add("can@10.0.0.7:2222", Some("box")))
        );
    }

    #[test]
    fn a_bad_user_or_port_stays_in_the_form() {
        for (values, error, focus) in [
            (
                ["me@vps", "can", "", ""],
                "the host already names a user",
                USER,
            ),
            (
                ["vps", "c an", "", ""],
                "user cannot have @ or spaces",
                USER,
            ),
            (
                ["vps:22", "", "2222", ""],
                "the host already names a port",
                PORT,
            ),
            (
                ["vps", "", "0", ""],
                "port must be a number from 1 to 65535",
                PORT,
            ),
            (
                ["vps", "", "ssh", ""],
                "port must be a number from 1 to 65535",
                PORT,
            ),
        ] {
            let mut form = filled(values);
            assert_eq!(form.submit(), None, "{values:?}");
            assert_eq!(form.error.as_deref(), Some(error));
            assert_eq!(form.focus, focus);
        }
    }

    #[test]
    fn tab_and_arrows_cycle_the_fields() {
        let mut form = AddForm::new();
        let press = |form: &mut AddForm, code| form.key(&KeyEvent::from(code));
        for expected in [USER, PORT, NAME, HOST] {
            press(&mut form, KeyCode::Tab);
            assert_eq!(form.focus, expected);
        }
        press(&mut form, KeyCode::Up);
        assert_eq!(form.focus, NAME);
    }
}
