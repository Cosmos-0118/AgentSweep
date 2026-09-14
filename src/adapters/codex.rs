use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::Value;

use super::{app_support, capture, fallback_home, process_running, Adapter};

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
        if let Some(json) = doctor_json() {
            if let Some(status) = json
                .pointer("/checks/app_server.status/details/status")
                .and_then(|v| v.as_str())
            {
                if status != "not running" && !status.is_empty() {
                    return true;
                }
            }
        }
        process_running(&["codex"])
    }
}

fn doctor_json() -> Option<Value> {
    DOCTOR
        .get_or_init(|| {
            let stdout = capture("codex", &["doctor", "--json"])?;
            let trimmed = stdout
                .lines()
                .skip_while(|l| !l.trim_start().starts_with('{'))
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str(&trimmed).ok()
        })
        .clone()
}
