use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::Item;

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod kiro;
pub mod opencode;
pub mod vscode;
pub mod windsurf;

pub use antigravity::Antigravity;
pub use claude::Claude;
pub use codex::Codex;
pub use cursor::Cursor;
pub use kiro::Kiro;
pub use opencode::OpenCode;
pub use vscode::VsCode;
pub use windsurf::Windsurf;

pub trait Adapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn detect(&self) -> bool;
    fn version(&self) -> Option<String>;
    fn roots(&self) -> BTreeMap<String, PathBuf>;
    fn is_running(&self) -> bool;
    /// Process descriptions used in the "quit the app" safety message.
    /// Most adapters can use their id as the process name; adapters with a
    /// branded executable can override this with a more precise matcher.
    fn running_processes(&self) -> Vec<String> {
        find_running(&[self.id()])
    }
    fn enrich(&self, _items: &mut Vec<Item>) {}
}

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Codex),
        Box::new(Claude),
        Box::new(OpenCode),
        Box::new(VsCode),
        Box::new(Cursor),
        Box::new(Windsurf),
        Box::new(Kiro),
        Box::new(Antigravity),
    ]
}

pub fn by_id(id: &str) -> Option<Box<dyn Adapter>> {
    all().into_iter().find(|a| a.id() == id)
}

pub fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

pub fn process_running(names: &[&str]) -> bool {
    !find_running(names).is_empty()
}

/// Human-readable descriptors (`name (pid N)`) of matched processes,
/// so refusal messages can say *what* is running.
pub fn find_running(names: &[&str]) -> Vec<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let self_pid = sysinfo::get_current_pid().ok();
    let mut out = Vec::new();
    for p in sys.processes().values() {
        if self_pid.is_some_and(|me| p.pid() == me) {
            continue;
        }
        let name = p.name().to_string_lossy().to_ascii_lowercase();
        let cmd = p
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let hit = names.iter().any(|n| {
            let n = n.to_ascii_lowercase();
            name == n
                || starts_with_boundary(&name, &n)
                || name.contains(&format!("{n}-"))
                || cmd.split_whitespace().any(|part| {
                    part == n
                        || part.ends_with(&format!("/{n}"))
                        || part.ends_with(&format!("\\{n}"))
                })
        });
        if hit {
            out.push(format!("{} (pid {})", p.name().to_string_lossy(), p.pid()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Find processes that belong to the official Visual Studio Code application.
///
/// Electron-based VS Code forks commonly give renderer processes names such as
/// `Code Helper`. Matching that generic name made Cursor, Windsurf, Kiro, and
/// other forks look like VS Code. On platforms that expose an executable path,
/// require the path to identify the official VS Code installation. If the OS
/// cannot provide that path, only the primary `Code` process is accepted.
pub fn find_vscode_processes() -> Vec<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let self_pid = sysinfo::get_current_pid().ok();
    let mut out = Vec::new();
    for p in sys.processes().values() {
        if self_pid.is_some_and(|me| p.pid() == me) {
            continue;
        }
        if is_vscode_process(&p.name().to_string_lossy(), p.exe()) {
            out.push(format!("{} (pid {})", p.name().to_string_lossy(), p.pid()));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn is_vscode_process(name: &str, executable: Option<&std::path::Path>) -> bool {
    let name = name.to_ascii_lowercase();
    let is_code_process = name == "code" || name.starts_with("code helper");
    if !is_code_process {
        return false;
    }
    match executable {
        Some(path) => is_official_vscode_path(path),
        // Avoid treating a fork's `Code Helper` as VS Code when the OS did
        // not expose a path to disambiguate it.
        None => name == "code",
    }
}

fn is_official_vscode_path(path: &std::path::Path) -> bool {
    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    path.contains("/visual studio code.app/")
        || path.contains("/microsoft vs code/")
        || path.starts_with("/usr/share/code/")
        || path.starts_with("/snap/code/")
        || path.contains("/.var/app/com.visualstudio.code/")
        || path.starts_with("/app/extra/vscode/")
}

pub fn fallback_home(rel: &str) -> PathBuf {
    crate::util::home_dir().join(rel)
}

/// The Electron/native "app data" directory for a desktop app named `name`:
/// `~/Library/Application Support/<name>` on macOS, `%APPDATA%\<name>` on
/// Windows, `~/.config/<name>` on Linux.
pub fn app_support(name: &str) -> PathBuf {
    let home = crate::util::home_dir();
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support").join(name)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join(name)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join(name)
    }
}

/// `name` starts with `prefix` and the next character (if any) is not
/// alphanumeric, so short tool ids like "code" don't accidentally match
/// unrelated processes like "codex".
fn starts_with_boundary(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    rest.chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_rejects_codex_for_code() {
        assert!(!starts_with_boundary("codex", "code"));
        assert!(starts_with_boundary("code helper (renderer)", "code"));
        assert!(starts_with_boundary("code", "code"));
        assert!(starts_with_boundary("cursor helper (gpu)", "cursor"));
    }

    #[test]
    fn vscode_match_requires_official_app_path_for_helpers() {
        assert!(is_vscode_process(
            "Code Helper (Renderer)",
            Some(std::path::Path::new(
                "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper",
            )),
        ));
        assert!(!is_vscode_process(
            "Code Helper (Renderer)",
            Some(std::path::Path::new(
                "/Applications/Cursor.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper",
            )),
        ));
        assert!(!is_vscode_process("Code Helper (GPU)", None));
    }

    #[test]
    fn vscode_primary_process_without_path_is_still_detected() {
        assert!(is_vscode_process("Code", None));
        assert!(!is_vscode_process("Cursor", None));
    }
}
