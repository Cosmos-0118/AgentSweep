use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{app_support, capture, fallback_home, process_running, Adapter};

pub struct Windsurf;

impl Adapter for Windsurf {
    fn id(&self) -> &'static str {
        "windsurf"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists()) || capture("windsurf", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("windsurf", &["--version"])
            .map(|s| s.lines().next().unwrap_or(s.trim()).trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("cli".into(), fallback_home(".windsurf"));
        map.insert("codeium".into(), fallback_home(".codeium"));
        map.insert("app".into(), app_support("Windsurf"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["windsurf"])
    }
}
