use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use super::{app_support, capture, fallback_home, process_running, Adapter};
use crate::model::{Item, Risk};
use crate::util;

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

    /// Individual `settings.json.bak.<timestamp>` filenames, so the generic
    /// unknown sweep doesn't also report them - `enrich` below is the one
    /// place that decides what happens to each of them.
    fn claimed_top_level(&self, root: &str) -> Vec<String> {
        if root != "home" {
            return vec![];
        }
        self.roots()
            .get("home")
            .map(|home| settings_backups(home).into_iter().map(|(name, _, _)| name).collect())
            .unwrap_or_default()
    }

    /// Anthropic's own docs say `backups/` contents are disposable, but a
    /// standalone `settings.json.bak.<timestamp>` is the only recovery path
    /// if settings.json gets corrupted. A blanket age-based rule would
    /// eventually prune the single newest backup too, leaving nothing to
    /// recover from - keep the newest unconditionally and only ever offer
    /// older ones for cleanup, the same "active + fallback" shape Codex uses
    /// for its standalone releases.
    fn enrich(&self, items: &mut Vec<Item>) {
        let Some(home) = self.roots().get("home").cloned() else {
            return;
        };
        let mut backups = settings_backups(&home);
        if backups.is_empty() {
            return;
        }
        backups.sort_by_key(|(_, _, mtime)| *mtime);
        let (newest_name, newest_path, newest_mtime) = backups.pop().unwrap();
        items.push(Item {
            rule_id: "claude.settings_backup_current".into(),
            tool: self.id().into(),
            label: format!("Newest settings backup: {newest_name}"),
            bytes: util::disk_usage_recursive(&newest_path),
            paths: vec![newest_path],
            risk: Risk::Critical,
            requires_stopped: false,
            consequence: "The most recent settings.json backup, kept as a recovery fallback if settings.json becomes corrupted. Never deleted.".into(),
            oldest_mtime: Some(newest_mtime),
            newest_mtime: Some(newest_mtime),
            delegate: None,
        });
        if backups.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = backups.iter().map(|(_, p, _)| p.clone()).collect();
        let bytes = paths.iter().map(|p| util::disk_usage_recursive(p)).sum();
        items.push(Item {
            rule_id: "claude.settings_backup_stale".into(),
            tool: self.id().into(),
            label: "Older settings backups".into(),
            bytes,
            paths,
            risk: Risk::Safe,
            requires_stopped: false,
            consequence: "Deletes settings.json backups other than the newest. The newest backup is always kept as a recovery fallback.".into(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        });
    }
}

fn settings_backups(home: &std::path::Path) -> Vec<(String, PathBuf, SystemTime)> {
    let Ok(rd) = fs::read_dir(home) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("settings.json.bak.") {
                return None;
            }
            let mtime = entry.metadata().ok()?.modified().ok()?;
            Some((name, entry.path(), mtime))
        })
        .collect()
}
