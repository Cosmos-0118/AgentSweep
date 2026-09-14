use std::collections::BTreeMap;
use std::path::PathBuf;

use super::composer_chat;
use super::fs_sessions;
use super::{app_support, capture, fallback_home, process_running, Adapter};
use crate::model::Item;

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

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        if let Some(cli) = roots.get("cli") {
            let session_roots = fs_sessions::resolve_roots(cli, &["sessions"]);
            fs_sessions::enrich_stale_children(
                items,
                "kiro.cli.sessions",
                "Kiro",
                "agent sessions",
                &session_roots,
            );
        }
        if let Some(app) = roots.get("app") {
            composer_chat::enrich_item(
                items,
                "kiro.app.chat_sessions",
                "Kiro",
                &composer_chat::chat_db(app),
            );
        }
    }
}
