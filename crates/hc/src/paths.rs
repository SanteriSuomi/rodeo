use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::env::Env;
use crate::error::HcError;

const HOLDER_ID: usize = 8;
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "macos") { 103 } else { 107 };
// Pipe names live in their own namespace and allow up to 256 characters.
#[cfg(windows)]
const MAX_SOCKET_PATH: usize = 256;

/// Pipe names carry the user so two users' cores never meet.
#[cfg(windows)]
fn pipe_prefix(env: &Env) -> String {
    let user: String = env.get("USERNAME").map_or(String::new(), |name| {
        name.to_string_lossy()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect()
    });
    format!("herdr-core-{user}-")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub session: String,
    pub socket: PathBuf,
    pub holder_socket: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

#[derive(Deserialize)]
struct Config {
    socket: Option<String>,
}

impl Paths {
    pub fn resolve(env: &Env) -> Result<Self, HcError> {
        #[cfg(unix)]
        let run_root = {
            let uid = rustix::process::getuid().as_raw();
            PathBuf::from(format!("/run/user/{uid}"))
        };
        // Windows has no runtime dir: pipes live in the pipe namespace.
        #[cfg(windows)]
        let run_root = PathBuf::from(r"\\.\pipe\");
        Self::resolve_with(env, &run_root)
    }

    fn resolve_with(env: &Env, run_user: &Path) -> Result<Self, HcError> {
        let session = session(env.get("HC_SESSION").map(OsStr::to_string_lossy).as_deref())?;
        let home = env.home();
        let xdg = |var: &str, fallback: PathBuf| env.nonempty(var).map_or(fallback, PathBuf::from);
        #[cfg(unix)]
        let (state_home, config_home) = (home.join(".local/state"), home.join(".config"));
        #[cfg(windows)]
        let (state_home, config_home) = (
            env.nonempty("LOCALAPPDATA")
                .map_or_else(|| home.join(r"AppData\Local"), PathBuf::from),
            env.nonempty("APPDATA")
                .map_or_else(|| home.join(r"AppData\Roaming"), PathBuf::from),
        );
        let state = xdg("XDG_STATE_HOME", state_home)
            .join("herdr-core")
            .join(&session);
        let config = xdg("XDG_CONFIG_HOME", config_home)
            .join("herdr-core")
            .join("config.toml");
        #[cfg(unix)]
        let run = env
            .nonempty("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| run_user.is_dir().then(|| run_user.to_path_buf()))
            .map_or_else(|| state.clone(), |run| run.join("herdr-core"));
        #[cfg(windows)]
        let run = run_user.to_path_buf();
        let chosen = env
            .nonempty("HC_SOCKET")
            .map(PathBuf::from)
            .or_else(|| configured_socket(&config));
        let (socket, holder_socket) = if let Some(socket) = chosen {
            (socket.clone(), socket.with_extension("holder.sock"))
        } else {
            #[cfg(unix)]
            let (socket, holder) = (
                run.join(format!("{session}.sock")),
                run.join(format!("{session}.holder.sock")),
            );
            #[cfg(windows)]
            let (socket, holder) = {
                let prefix = pipe_prefix(env);
                (
                    run.join(format!("{prefix}{session}.sock")),
                    run.join(format!("{prefix}{session}.holder.sock")),
                )
            };
            (socket, holder)
        };
        let longest = holder_socket.as_os_str().len() + 1 + HOLDER_ID;
        if socket.as_os_str().len().max(longest) > MAX_SOCKET_PATH {
            return Err(HcError::Paths(format!(
                "the socket path for session {session:?} is too long here ({longest} bytes; this system allows {MAX_SOCKET_PATH})"
            )));
        }
        Ok(Self {
            session,
            socket,
            holder_socket,
            state,
            config,
        })
    }

    pub fn listening_at(&self) -> Option<PathBuf> {
        std::fs::read_to_string(self.state.join("core.lock"))
            .ok()
            .map(PathBuf::from)
    }
}

fn configured_socket(config: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config).ok()?;
    toml::from_str::<Config>(&text)
        .ok()?
        .socket
        .map(PathBuf::from)
}

fn session(raw: Option<&str>) -> Result<String, HcError> {
    let name = raw
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("default");
    if name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || r#"/\:*?"<>|"#.contains(c))
    {
        return Err(HcError::Paths(format!(
            "HC_SESSION {name:?} can't be a folder name"
        )));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> Env {
        vars.iter().copied().collect()
    }

    fn resolve(vars: &[(&str, &str)]) -> Paths {
        Paths::resolve_with(&env(vars), Path::new("/nonexistent/run/user/1")).expect("paths")
    }

    #[cfg(unix)]
    #[test]
    fn defaults_live_under_home() {
        let paths = resolve(&[("HOME", "/h")]);
        assert_eq!(paths.session, "default");
        assert_eq!(
            paths.state,
            PathBuf::from("/h/.local/state/herdr-core/default")
        );
        assert_eq!(
            paths.config,
            PathBuf::from("/h/.config/herdr-core/config.toml")
        );
        assert_eq!(
            paths.socket,
            PathBuf::from("/h/.local/state/herdr-core/default/default.sock")
        );
        assert_eq!(
            paths.holder_socket,
            PathBuf::from("/h/.local/state/herdr-core/default/default.holder.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_dir_holds_the_socket_per_session() {
        let paths = resolve(&[
            ("HOME", "/h"),
            ("HC_SESSION", " t "),
            ("XDG_RUNTIME_DIR", "/r"),
            ("XDG_STATE_HOME", "/s"),
        ]);
        assert_eq!(paths.session, "t");
        assert_eq!(paths.socket, PathBuf::from("/r/herdr-core/t.sock"));
        assert_eq!(paths.state, PathBuf::from("/s/herdr-core/t"));
    }

    #[cfg(unix)]
    #[test]
    fn run_user_counts_only_when_it_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::resolve_with(&env(&[("HOME", "/h"), ("XDG_RUNTIME_DIR", "")]), dir.path())
                .expect("paths");
        assert_eq!(paths.socket, dir.path().join("herdr-core/default.sock"));
    }

    #[cfg(unix)]
    #[test]
    fn hc_socket_wins_over_config_and_config_over_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("herdr-core");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(
            config.join("config.toml"),
            "headless = \"80x24\"\nsocket = \"/c/core.sock\"\n",
        )
        .expect("config");
        let root = dir.path().to_str().expect("utf8");
        let base = [
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", root),
            ("XDG_RUNTIME_DIR", "/r"),
        ];
        let paths = resolve(&base);
        assert_eq!(paths.socket, PathBuf::from("/c/core.sock"));
        assert_eq!(paths.holder_socket, PathBuf::from("/c/core.holder.sock"));
        let mut chosen = base.to_vec();
        chosen.push(("HC_SOCKET", "/e/x.sock"));
        assert_eq!(resolve(&chosen).socket, PathBuf::from("/e/x.sock"));
        std::fs::write(config.join("config.toml"), "socket = [").expect("broken");
        assert_eq!(
            resolve(&base).socket,
            PathBuf::from("/r/herdr-core/default.sock")
        );
    }

    #[test]
    fn sessions_must_be_folder_names() {
        for bad in ["a/b", r"a\b", "a:b", "..", ".", "a\tb"] {
            assert!(
                Paths::resolve_with(&env(&[("HC_SESSION", bad)]), Path::new("/x")).is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(resolve(&[("HC_SESSION", "  ")]).session, "default");
    }

    #[cfg(unix)]
    #[test]
    fn long_socket_paths_are_refused() {
        let long = format!("/{}", "x".repeat(100));
        let result = Paths::resolve_with(&env(&[("XDG_RUNTIME_DIR", &long)]), Path::new("/x"));
        assert!(matches!(result, Err(HcError::Paths(_))));
    }

    #[cfg(windows)]
    fn try_pipes(vars: &[(&str, &str)]) -> Result<Paths, HcError> {
        Paths::resolve_with(&env(vars), Path::new(r"\\.\pipe\"))
    }

    #[cfg(windows)]
    fn pipes(vars: &[(&str, &str)]) -> Paths {
        try_pipes(vars).expect("pipes")
    }

    #[cfg(windows)]
    #[test]
    fn pipes_are_named_after_the_user_and_the_session() {
        let paths = pipes(&[("USERNAME", "sants"), ("HC_SESSION", " t ")]);
        assert_eq!(paths.session, "t");
        assert_eq!(
            paths.socket,
            PathBuf::from(r"\\.\pipe\herdr-core-sants-t.sock")
        );
        assert_eq!(
            paths.holder_socket,
            PathBuf::from(r"\\.\pipe\herdr-core-sants-t.holder.sock")
        );
        // Only safe characters survive the user name; a missing one still makes
        // a pipe, just one that does not say whose it is.
        assert_eq!(
            pipes(&[("USERNAME", r"a\b:c")]).socket,
            PathBuf::from(r"\\.\pipe\herdr-core-abc-default.sock")
        );
        assert_eq!(
            pipes(&[]).socket,
            PathBuf::from(r"\\.\pipe\herdr-core--default.sock")
        );
    }

    #[cfg(windows)]
    #[test]
    fn defaults_live_in_the_appdata_folders() {
        let paths = pipes(&[("USERPROFILE", r"C:\Users\sants")]);
        assert_eq!(
            paths.state,
            PathBuf::from(r"C:\Users\sants\AppData\Local\herdr-core\default")
        );
        assert_eq!(
            paths.config,
            PathBuf::from(r"C:\Users\sants\AppData\Roaming\herdr-core\config.toml")
        );
        let paths = pipes(&[
            ("USERPROFILE", r"C:\Users\sants"),
            ("LOCALAPPDATA", r"D:\state"),
            ("APPDATA", r"D:\config"),
        ]);
        assert_eq!(paths.state, PathBuf::from(r"D:\state\herdr-core\default"));
        assert_eq!(
            paths.config,
            PathBuf::from(r"D:\config\herdr-core\config.toml")
        );
        // The XDG vars still win, which is how a test core stays in its temp dir.
        assert_eq!(
            pipes(&[
                ("LOCALAPPDATA", r"D:\state"),
                ("XDG_STATE_HOME", "E:\\test")
            ])
            .state,
            PathBuf::from(r"E:\test\herdr-core\default")
        );
    }

    #[cfg(windows)]
    #[test]
    fn hc_socket_wins_over_config_and_config_over_the_pipe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("herdr-core");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(config.join("config.toml"), r#"socket = "C:\\core.sock""#).expect("config");
        let root = dir.path().to_str().expect("utf8");
        let base = [("APPDATA", root)];
        let paths = pipes(&base);
        assert_eq!(paths.socket, PathBuf::from(r"C:\core.sock"));
        assert_eq!(paths.holder_socket, PathBuf::from(r"C:\core.holder.sock"));
        let mut chosen = base.to_vec();
        chosen.push(("HC_SOCKET", r"C:\x.sock"));
        assert_eq!(pipes(&chosen).socket, PathBuf::from(r"C:\x.sock"));
        std::fs::write(config.join("config.toml"), "socket = [").expect("broken");
        assert_eq!(
            pipes(&base).socket,
            PathBuf::from(r"\\.\pipe\herdr-core--default.sock")
        );
    }

    #[cfg(windows)]
    #[test]
    fn long_pipe_names_are_refused() {
        // \\.\pipe\ + herdr-core-sants- + session + .holder.sock, then the
        // separator and the holder id: 47 fixed bytes around the session.
        let fits = "x".repeat(256 - 47);
        let paths = pipes(&[("USERNAME", "sants"), ("HC_SESSION", &fits)]);
        assert_eq!(paths.holder_socket.as_os_str().len() + 9, 256);
        let too_long = "x".repeat(256 - 46);
        assert!(matches!(
            try_pipes(&[("USERNAME", "sants"), ("HC_SESSION", &too_long)]),
            Err(HcError::Paths(_))
        ));
    }
}
