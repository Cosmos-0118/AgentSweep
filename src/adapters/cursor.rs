use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{app_support, capture, fallback_home, process_running, Adapter};

pub struct Cursor;

impl Adapter for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists())
            || capture("cursor-agent", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("cursor-agent", &["--version"]).map(|s| s.trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("cli".into(), fallback_home(".cursor"));
        map.insert("app".into(), app_support("Cursor"));
        map.insert("dotfile".into(), fallback_home(".cursor_info"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["cursor"])
    }
}
