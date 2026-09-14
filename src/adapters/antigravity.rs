use std::collections::BTreeMap;
use std::path::PathBuf;

use super::composer_chat;
use super::{app_support, capture, fallback_home, process_running, Adapter};
use crate::model::Item;

pub struct Antigravity;

impl Adapter for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists())
            || capture("antigravity", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        capture("antigravity", &["--version"]).map(|s| s.trim().to_string())
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        map.insert("cli".into(), fallback_home(".antigravity"));
        map.insert("ide".into(), fallback_home(".antigravity-ide"));
        map.insert("cockpit".into(), fallback_home(".antigravity_cockpit"));
        map.insert("app".into(), app_support("Antigravity"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["antigravity"])
    }

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        if let Some(app) = roots.get("app") {
            composer_chat::enrich_item(
                items,
                "antigravity.app.chat_sessions",
                "Antigravity",
                &composer_chat::chat_db(app),
            );
        }
    }
}
