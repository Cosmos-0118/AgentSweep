use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{app_support, capture, fallback_home, process_running, Adapter};

pub struct Kiro;

impl Adapter for Kiro {
    fn id(&self) -> &'static str {
        "kiro"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists()) || capture("kiro", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("kiro", &["--version"]).map(|s| s.trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("cli".into(), fallback_home(".kiro"));
        map.insert("app".into(), app_support("Kiro"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["kiro"])
    }
}
