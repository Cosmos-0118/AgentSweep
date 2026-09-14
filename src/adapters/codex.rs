use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::{app_support, capture, capture_with_timeout, fallback_home, process_running, Adapter};

static DOCTOR: OnceLock<Option<Value>> = OnceLock::new();
static VERSION: OnceLock<Option<String>> = OnceLock::new();

pub struct Codex;

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        self.roots().get("home").is_some_and(|p| p.exists())
            || capture("codex", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        VERSION
            .get_or_init(|| {
                if let Some(json) = doctor_json() {
                    if let Some(v) = json.get("codexVersion").and_then(|v| v.as_str()) {
                        return Some(v.to_string());
                    }
                }
                capture("codex", &["--version"]).map(|s| {
                    s.lines()
                        .find(|l| !l.to_ascii_lowercase().contains("warning"))
                        .unwrap_or(s.trim())
                        .trim()
                        .to_string()
                })
            })
            .clone()
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        let home = doctor_json()
            .and_then(|j| {
                j.pointer("/checks/config.load/details/CODEX_HOME")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
            })
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| fallback_home(".codex"));
        map.insert("home".into(), home);
        map.insert("desktop".into(), app_support("Codex"));
        map
    }

    fn is_running(&self) -> bool {
        // This used to shell out to `codex doctor --json` for the background
        // app-server daemon's live status, since a daemon can run under a
        // process name a plain process-list check wouldn't recognize. On a
        // real machine that command measured 7-8s wall time, of which the
        // app-server check itself accounted for 0ms - the rest is fixed CLI
        // startup plus two unrelated network round-trips the doctor also
        // runs. There is no timeout short enough to make that probe both
        // fast and useful: anything under ~7s times out every single time,
        // so it was paying its budget on every periodic refresh for a
        // result that structurally never arrives. `process_running` matches
        // on the full command line, not just the process name, so a daemon
        // started as `codex app-server ...` is still caught by the "codex"
        // token in its argv - drop the doctor probe and rely on that alone.
        process_running(&["codex"])
    }
}

fn doctor_json() -> Option<Value> {
    DOCTOR.get_or_init(doctor_json_fresh).clone()
}

fn doctor_json_fresh() -> Option<Value> {
    doctor_json_with_timeout(Duration::from_secs(3))
}

fn doctor_json_with_timeout(timeout: Duration) -> Option<Value> {
    let stdout = capture_with_timeout("codex", &["doctor", "--json"], timeout)?;
    let trimmed = stdout
        .lines()
        .skip_while(|line| !line.trim_start().starts_with('{'))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&trimmed).ok()
}
