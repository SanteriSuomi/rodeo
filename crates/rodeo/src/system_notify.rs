use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(10);

#[cfg(windows)]
pub fn commands(title: &str, body: &str) -> Vec<Vec<String>> {
    vec![vec![
        "powershell".to_owned(),
        "-NoProfile".to_owned(),
        "-Command".to_owned(),
        toast_ps(title, body),
    ]]
}

#[cfg(not(windows))]
pub fn commands(title: &str, body: &str) -> Vec<Vec<String>> {
    let own = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        let script = format!(
            "display notification {} with title {}",
            apple_string(body),
            apple_string(title)
        );
        vec![
            own(&[
                "terminal-notifier",
                "-title",
                title,
                "-message",
                body,
                "-group",
                "rodeo",
            ]),
            own(&["osascript", "-e", &script]),
        ]
    } else {
        vec![
            own(&["notify-send", "--app-name=rodeo", "--", title, body]),
            own(&[
                "gdbus",
                "call",
                "--session",
                "--dest=org.freedesktop.Notifications",
                "--object-path=/org/freedesktop/Notifications",
                "--method=org.freedesktop.Notifications.Notify",
                "rodeo",
                "0",
                "",
                title,
                body,
                "[]",
                "{}",
                "-1",
            ]),
        ]
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One value as a PowerShell single-quoted literal. Mirrors the `app` module's
/// private `ps_quote`; the toast script is the only user outside `app`.
#[cfg(windows)]
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A WinRT toast through PowerShell: the PowerShell console host is STA, which
/// the toast API needs, and the AUMID of the stock PowerShell install lets the
/// notification appear without registering an application.
#[cfg(windows)]
fn toast_ps(title: &str, body: &str) -> String {
    // title/body: XML-escaped for the toast body, then ps-quoted for the
    // PowerShell single-quoted XML string below.
    format!(
        r#"$ErrorActionPreference = 'Stop'
try {{
  [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
  [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom, ContentType = WindowsRuntime] > $null
  $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
  $xml.LoadXml('<toast><visual><binding template="ToastText02"><text>{}</text><text>{}</text></binding></visual></toast>')
  [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\WindowsPowerShell\v1.0\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($xml))
  exit 0
}} catch {{ exit 1 }}"#,
        ps_quote(&xml_escape(title)),
        ps_quote(&xml_escape(body)),
    )
}

#[cfg_attr(windows, allow(dead_code))]
fn apple_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

pub fn show(title: &str, body: &str) {
    let commands = commands(&clean(title), &clean(body));
    let spawned = std::thread::Builder::new()
        .name("rodeo-notify".into())
        .spawn(move || {
            for argv in commands {
                if run(&argv) {
                    return;
                }
            }
            tracing::debug!("no system notification service answered");
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot start the notification thread");
    }
}

fn run(argv: &[String]) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return false;
    };
    let Ok(mut child) = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < LIMIT => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_carry_the_text_as_arguments() {
        let commands = commands("claude finished", "repo · 1");
        // Windows has the single toast candidate; macOS/Linux keep the
        // two-candidate fallback list.
        assert!(commands.len() >= if cfg!(windows) { 1 } else { 2 });
        for argv in &commands {
            assert!(
                argv.iter().any(|a| a.contains("claude finished")),
                "{argv:?}"
            );
            assert!(argv.iter().any(|a| a.contains("repo · 1")), "{argv:?}");
        }
        assert_eq!(apple_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(clean("a\x1b]0;x\x07b"), "a]0;xb");
    }

    #[test]
    fn xml_escape_covers_the_markup_characters() {
        assert_eq!(xml_escape("a&<b>"), "a&amp;&lt;b&gt;");
    }

    #[cfg(windows)]
    #[test]
    fn toast_script_quotes_and_escapes() {
        let commands = commands("it's <done>", "");
        assert_eq!(commands.len(), 1);
        let argv = &commands[0];
        assert_eq!(argv[0], "powershell");
        let script = argv
            .iter()
            .find(|a| a.contains("ToastNotificationManager"))
            .expect("the toast script");
        assert!(script.contains("'it''s &lt;done&gt;'"), "{script}");
        assert!(
            !script.replace("'it''s &lt;done&gt;'", "").contains("it's"),
            "{script}"
        );
    }
}
