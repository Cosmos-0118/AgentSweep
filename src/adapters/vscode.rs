use std::collections::BTreeMap;
use std::path::PathBuf;

use super::copilot_chat;
use super::{app_support, capture, fallback_home, find_vscode_processes, Adapter};
use crate::model::Item;

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
        !find_vscode_processes().is_empty()
    }

    fn running_processes(&self) -> Vec<String> {
        find_vscode_processes()
    }

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        if let Some(app) = roots.get("app") {
            copilot_chat::enrich_item(items, "vscode.app.chat_sessions", app);
        }
    }
}

pub fn delete_stale_sessions(app_root: &std::path::Path) -> anyhow::Result<u64> {
    copilot_chat::delete_stale_sessions(app_root)
}
