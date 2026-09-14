use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{app_support, capture, fallback_home, process_running, Adapter};

pub struct Claude;

impl Adapter for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists()) || capture("claude", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("claude", &["--version"]).map(|s| s.trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("home".into(), fallback_home(".claude"));
        map.insert("dotfile".into(), fallback_home(".claude.json"));
        map.insert("desktop".into(), app_support("Claude"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["claude"])
    }
}
