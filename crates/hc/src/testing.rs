use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::env::Env;
use crate::paths::Paths;
use crate::start::{find_hc, start_server};

const SESSION: &str = "t";
const STRAGGLERS: Duration = Duration::from_secs(2);

/// The temp folder as a core can name it: canonical, so a socket path built
/// from it matches what the core resolves. Windows canonicalization answers
/// with a `\\?\` device path, which a pane told to `cd` there cannot read, so
/// the prefix comes back off.
fn temp_root() -> PathBuf {
    resolved(&std::env::temp_dir())
}

fn resolved(base: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(base).expect("the temp dir resolves");
    #[cfg(windows)]
    if let Some(plain) = resolved
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        return PathBuf::from(plain);
    }
    resolved
}

pub struct TestCore {
    root: tempfile::TempDir,
    env: Env,
    bin: PathBuf,
}

impl std::fmt::Debug for TestCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestCore")
            .field("root", &self.root.path())
            .finish_non_exhaustive()
    }
}

impl TestCore {
    pub async fn new() -> Self {
        Self::new_under(temp_root()).await
    }

    /// A core whose temp folder sits under `base` instead of the temp root.
    /// TUI harnesses read path previews off a fixed-width screen, so they need
    /// the core's home short; deep temp roots (/var/folders/… on macOS, agent
    /// scratch folders) push those previews off the panel.
    async fn new_under(base: PathBuf) -> Self {
        let root = tempfile::Builder::new()
            .prefix("hc")
            .tempdir_in(base)
            .expect("a temp dir for the test core");
        let path = root.path();
        // A Windows pipe is named only by user and session, so parallel test
        // cores have to differ in the session; the temp folder name already is
        // unique. On unix the socket lives in the temp folder, so one name fits.
        #[cfg(windows)]
        let session = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(SESSION)
            .to_owned();
        #[cfg(unix)]
        let session = SESSION.to_owned();
        let env = Env::new()
            .with("PATH", std::env::var_os("PATH").unwrap_or_default())
            .with("HC_SESSION", &session)
            .with("HC_SETTLE_MS", "0")
            .with("XDG_RUNTIME_DIR", path.join("run"))
            .with("XDG_STATE_HOME", path.join("state"))
            .with("XDG_CONFIG_HOME", path.join("config"));
        #[cfg(unix)]
        let env = env.with("HOME", path).with("SHELL", "/bin/sh");
        // Windows has no HOME and hc picks its own shell there, so the core gets
        // the profile vars it does read. USERNAME names the pipe, SYSTEMROOT and
        // COMSPEC keep spawned programs alive, and LOCALAPPDATA/APPDATA pin the
        // state and config folders inside the temp root like the XDG vars do.
        #[cfg(windows)]
        let env = env
            .with("USERPROFILE", path)
            .with("USERNAME", std::env::var_os("USERNAME").unwrap_or_default())
            .with(
                "COMPUTERNAME",
                std::env::var_os("COMPUTERNAME").unwrap_or_default(),
            )
            .with(
                "SYSTEMROOT",
                std::env::var_os("SYSTEMROOT").unwrap_or_default(),
            )
            .with("COMSPEC", std::env::var_os("COMSPEC").unwrap_or_default())
            .with("TEMP", path)
            .with("TMP", path)
            .with("LOCALAPPDATA", path.join("state"))
            .with("APPDATA", path.join("config"));
        let bin = find_hc(&env)
            .await
            .expect("hc must be installed for live hc tests");
        Self { root, env, bin }
    }

    pub async fn start() -> Self {
        Self::start_with(|_, _| {}).await
    }

    pub async fn start_with(prepare: impl FnOnce(&Path, &mut Env)) -> Self {
        Self::start_under(temp_root(), prepare).await
    }

    /// `start_with` under a chosen base folder, resolved like the temp root.
    pub async fn start_under(
        base: impl AsRef<Path>,
        prepare: impl FnOnce(&Path, &mut Env),
    ) -> Self {
        let mut core = Self::new_under(resolved(base.as_ref())).await;
        prepare(core.root.path(), &mut core.env);
        if let Err(error) = start_server(&core.bin, &core.env).await {
            panic!("hc server start: {error}");
        }
        core
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub const fn env(&self) -> &Env {
        &self.env
    }

    pub fn paths(&self) -> Paths {
        Paths::resolve(&self.env).expect("test core paths")
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    pub async fn client(&self) -> Client {
        match Client::connect_env(&self.env).await {
            Ok(client) => client,
            Err(error) => panic!("connect to the test core: {error}"),
        }
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(self.env.iter());
        command
    }

    pub fn hc(&self) -> Command {
        self.command(&self.bin)
    }

    fn stop(&self) -> Vec<String> {
        let _ = self
            .hc()
            .args(["server", "stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        stragglers(self)
    }
}

/// Processes the core left behind. On unix every test process carries the
/// session's XDG_STATE_HOME in its environment, so /proc finds them all.
#[cfg(unix)]
fn stragglers(core: &TestCore) -> Vec<String> {
    let marker = format!("XDG_STATE_HOME={}", core.root().join("state").display());
    let started = Instant::now();
    while !holding(&marker).is_empty() && started.elapsed() < STRAGGLERS {
        std::thread::sleep(Duration::from_millis(50));
    }
    holding(&marker)
        .into_iter()
        .map(|pid| {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
            format!(
                "pid {pid} ({})",
                String::from_utf8_lossy(&cmdline).replace('\0', " ").trim()
            )
        })
        .collect()
}

/// Windows has no /proc to sweep, so the core is asked for its own pid and only
/// that pid is ended — never the binary by name, which would take the user's
/// own core with it. Terminals the core leaves behind stay undetected here.
#[cfg(windows)]
fn stragglers(core: &TestCore) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Status {
        pid: Option<u32>,
    }
    let started = Instant::now();
    loop {
        let Ok(out) = core
            .hc()
            .args(["server", "status", "--json"])
            .stdin(Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        // status exits non-zero once the pipe is gone, which is the clean exit.
        if !out.status.success() {
            return Vec::new();
        }
        if started.elapsed() >= STRAGGLERS {
            let Some(pid) = serde_json::from_slice::<Status>(&out.stdout)
                .ok()
                .and_then(|status| status.pid)
            else {
                return Vec::new();
            };
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            return vec![format!("pid {pid} (the test core outlived hc server stop)")];
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for TestCore {
    fn drop(&mut self) {
        let leaks = self.stop();
        assert!(
            leaks.is_empty() || std::thread::panicking(),
            "processes outlived hc server stop:\n{}",
            leaks.join("\n")
        );
    }
}

/// Test processes are found by their environment on unix only.
#[cfg(unix)]
fn holding(marker: &str) -> Vec<u32> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let marker = marker.as_bytes();
    procs
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|environ| environ.split(|b| *b == 0).any(|var| var == marker))
        })
        .collect()
}
