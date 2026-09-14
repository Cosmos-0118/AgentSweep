use std::collections::BTreeMap;
use std::path::PathBuf;

use super::composer_chat;
use super::{app_support, capture, fallback_home, process_running, Adapter};
use crate::model::Item;

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

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        if let Some(app) = roots.get("app") {
            composer_chat::enrich_item(
                items,
                "cursor.app.chat_sessions",
                "Cursor",
                &composer_chat::chat_db(app),
            );
        }
        if let Some(cli) = roots.get("cli") {
            enrich_cli_transcripts(items, cli);
        }
    }
}

fn enrich_cli_transcripts(items: &mut [Item], cli_root: &std::path::Path) {
    let Some(item) = items
        .iter_mut()
        .find(|i| i.rule_id == "cursor.cli.sessions")
    else {
        return;
    };
    if item.bytes == 0 {
        return;
    }
    let count = count_transcript_sessions(cli_root);
    if count > 0 {
        item.consequence = format!(
            "~/.cursor/projects includes {count} cursor-agent transcript session(s) plus terminals/MCP/asset state. Deleting removes those local copies; IDE Composer history in state.vscdb is separate."
        );
    }
}

fn count_transcript_sessions(cli_root: &std::path::Path) -> usize {
    let projects = cli_root.join("projects");
    if !projects.is_dir() {
        return 0;
    }
    let mut count = 0usize;
    let Ok(projects_iter) = std::fs::read_dir(&projects) else {
        return 0;
    };
    for project in projects_iter.flatten() {
        let transcripts = project.path().join("agent-transcripts");
        if !transcripts.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&transcripts) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                count += 1;
            }
        }
    }
    count
}

pub fn stale_session_ids(db: &std::path::Path) -> anyhow::Result<Vec<String>> {
    composer_chat::stale_session_ids(db)
}

pub fn delete_stale_sessions(db: &std::path::Path) -> anyhow::Result<u64> {
    composer_chat::delete_stale_sessions(db, "Cursor")
}
