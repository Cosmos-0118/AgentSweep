//! Age-filter filesystem session trees (Windsurf Cascade, Kiro agent sessions).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::composer_chat::SESSION_RETENTION_DAYS;
use crate::model::Item;
use crate::util;

/// Restrict a userdata session item to child entries inactive for 30+ days.
///
/// `roots` are absolute directories whose immediate children are sessions
/// (files or directories). When no stale children remain, bytes become 0.
pub fn enrich_stale_children(
    items: &mut [Item],
    rule_id: &str,
    product: &str,
    kind: &str,
    roots: &[PathBuf],
) {
    let Some(item) = items.iter_mut().find(|i| i.rule_id == rule_id) else {
        return;
    };
    let cutoff = retention_cutoff();
    let mut stale_paths = Vec::new();
    let mut bytes = 0u64;
    let mut count = 0usize;
    let mut oldest: Option<SystemTime> = None;
    let mut newest: Option<SystemTime> = None;

    for root in roots {
        if !root.exists() {
            continue;
        }
        if root.is_file() {
            if let Some(mt) = util::path_mtime(root) {
                if mt <= cutoff {
                    bytes = bytes.saturating_add(util::disk_usage(root));
                    count += 1;
                    stale_paths.push(root.clone());
                    merge_time(&mut oldest, &mut newest, mt);
                }
            }
            continue;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(mt) = util::path_mtime(&path) else {
                continue;
            };
            if mt > cutoff {
                continue;
            }
            let size = util::disk_usage(&path);
            if size == 0 {
                continue;
            }
            bytes = bytes.saturating_add(size);
            count += 1;
            stale_paths.push(path);
            merge_time(&mut oldest, &mut newest, mt);
        }
    }

    item.paths = stale_paths;
    item.bytes = bytes;
    item.oldest_mtime = oldest;
    item.newest_mtime = newest;
    if count == 0 {
        item.bytes = 0;
        return;
    }
    let oldest_days = oldest.map(util::age_days).map(|d| d as u32).unwrap_or(0);
    item.consequence = format!(
        "{product} reports {count} {kind} inactive for {SESSION_RETENTION_DAYS}+ days (oldest {oldest_days} days). Deep clean removes only those session files. Recent sessions stay."
    );
}

fn retention_cutoff() -> SystemTime {
    SystemTime::now() - std::time::Duration::from_secs((SESSION_RETENTION_DAYS as u64) * 86_400)
}

fn merge_time(oldest: &mut Option<SystemTime>, newest: &mut Option<SystemTime>, t: SystemTime) {
    *oldest = Some(oldest.map_or(t, |o| o.min(t)));
    *newest = Some(newest.map_or(t, |n| n.max(t)));
}

/// Resolve relative session roots under an adapter root.
pub fn resolve_roots(base: &Path, rels: &[&str]) -> Vec<PathBuf> {
    rels.iter().map(|r| base.join(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Risk;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn item(rule_id: &str) -> Item {
        Item {
            rule_id: rule_id.into(),
            tool: "t".into(),
            label: "sessions".into(),
            paths: vec![],
            bytes: 1,
            risk: Risk::Userdata,
            requires_stopped: true,
            consequence: String::new(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        }
    }

    #[test]
    fn enrich_keeps_only_stale_children() {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "agentsweep-fs-sessions-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old-session");
        let recent = dir.join("recent-session");
        fs::write(&old, vec![0u8; 100]).unwrap();
        fs::write(&recent, vec![0u8; 100]).unwrap();

        let old_mtime = SystemTime::now()
            - std::time::Duration::from_secs(((SESSION_RETENTION_DAYS as u64) + 5) * 86_400);
        let f = fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(old_mtime).unwrap();
        drop(f);

        let mut items = vec![item("t.sessions")];
        enrich_stale_children(&mut items, "t.sessions", "Test", "sessions", &[dir.clone()]);
        assert_eq!(items[0].paths, vec![old]);
        assert!(items[0].bytes > 0);
        let _ = fs::remove_dir_all(dir);
    }
}
