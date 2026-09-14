use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    let output = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
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
}
