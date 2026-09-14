use std::collections::BTreeMap;
use std::path::PathBuf;

use super::composer_chat;
use super::fs_sessions;
use super::{app_support, capture, fallback_home, process_running, Adapter};
use crate::model::Item;

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

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        if let Some(codeium) = roots.get("codeium") {
            let cascade_roots =
                fs_sessions::resolve_roots(codeium, &["cascade", "windsurf/cascade"]);
            fs_sessions::enrich_stale_children(
                items,
                "windsurf.codeium.cascade",
                "Windsurf",
                "Cascade sessions",
                &cascade_roots,
            );
        }
        if let Some(app) = roots.get("app") {
            composer_chat::enrich_item(
                items,
                "windsurf.app.chat_sessions",
                "Windsurf",
                &composer_chat::chat_db(app),
            );
        }
    }
}
