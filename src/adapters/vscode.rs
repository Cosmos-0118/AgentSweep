use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{app_support, capture, fallback_home, process_running, Adapter};

pub struct VsCode;

impl Adapter for VsCode {
    fn id(&self) -> &'static str {
        "vscode"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists()) || capture("code", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("code", &["--version"])
            .map(|s| s.lines().next().unwrap_or(s.trim()).trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("cli".into(), fallback_home(".vscode"));
        map.insert("app".into(), app_support("Code"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["code"])
    }
}
