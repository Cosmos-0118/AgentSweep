use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::Connection;

use super::{capture, fallback_home, process_running, Adapter};
use crate::model::{Item, Risk};
use crate::util;

static PATHS: OnceLock<Option<BTreeMap<String, PathBuf>>> = OnceLock::new();
static OCODE_VERSION: OnceLock<Option<String>> = OnceLock::new();

pub struct OpenCode;

impl Adapter for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self) -> bool {
        self.roots().values().any(|p| p.exists()) || capture("opencode", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        OCODE_VERSION
            .get_or_init(|| capture("opencode", &["--version"]).map(|s| s.trim().to_string()))
            .clone()
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        if let Some(map) = debug_paths_cached() {
            // `opencode debug paths` also reports `home`, `bin` and `tmp`.
            // Those are not OpenCode storage; scanning them for unknowns
            // would walk the entire $HOME. Keep only real storage roots.
            return map
                .into_iter()
                .filter(|(k, _)| {
                    matches!(
                        k.as_str(),
                        "data" | "cache" | "config" | "state" | "log" | "repos" | "tmp"
                    )
                })
                .collect();
        }
        let mut map = BTreeMap::new();
        map.insert("data".into(), fallback_home(".local/share/opencode"));
        map.insert("cache".into(), fallback_home(".cache/opencode"));
        map.insert("config".into(), fallback_home(".config/opencode"));
        map.insert("state".into(), fallback_home(".local/state/opencode"));
        map.insert("log".into(), fallback_home(".local/share/opencode/log"));
        map.insert("repos".into(), fallback_home(".local/share/opencode/repos"));
        map.insert("tmp".into(), std::env::temp_dir().join("opencode"));
        map
    }

    fn is_running(&self) -> bool {
        process_running(&["opencode"])
    }

    fn enrich(&self, items: &mut Vec<Item>) {
        let roots = self.roots();
        let Some(data) = roots.get("data") else {
            return;
        };
        let db = data.join("opencode.db");
        enrich_sessions(items, &db);
        enrich_legacy(items, data, &db);
    }
}

fn debug_paths_cached() -> Option<BTreeMap<String, PathBuf>> {
    PATHS
        .get_or_init(|| {
            let stdout = capture("opencode", &["debug", "paths"])?;
            let mut map = BTreeMap::new();
            for line in stdout.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let key = parts.next()?;
                let path = parts.next()?;
                map.insert(key.to_string(), PathBuf::from(path));
            }
            if map.is_empty() {
                return None;
            }
            Some(map)
        })
        .clone()
}

fn enrich_sessions(items: &mut [Item], db: &Path) {
    let Some(item) = items.iter_mut().find(|i| i.rule_id == "opencode.sessions") else {
        return;
    };
    if !db.exists() {
        item.bytes = 0;
        return;
    }
    match session_stats(db) {
        Ok(stats) => {
            item.bytes = stats.reclaimable.max(1);
            item.paths = vec![db.to_path_buf()];
            if stats.count == 0 {
                item.bytes = 0;
            }
            item.consequence = format!(
                "OpenCode reports {} sessions (oldest {} days). Deleting old sessions uses OpenCode's own command, then VACUUMs the database. Conversations selected for deletion cannot be resumed.",
                stats.count,
                stats.oldest_days
            );
        }
        Err(_) => {
            item.bytes = util::disk_usage(db);
            item.paths = vec![db.to_path_buf()];
        }
    }
}

fn enrich_legacy(items: &mut [Item], data: &Path, db: &Path) {
    let Some(item) = items
        .iter_mut()
        .find(|i| i.rule_id == "opencode.legacy_storage")
    else {
        return;
    };
    let storage = data.join("storage");
    if !storage.exists() {
        return;
    }
    let legacy_ids = count_legacy_session_ids(&storage);
    let db_ids = db_session_ids(db).unwrap_or_default();
    let unmatched: Vec<String> = legacy_ids
        .into_iter()
        .filter(|id| !db_ids.contains(id))
        .collect();
    if !unmatched.is_empty() {
        item.risk = Risk::Critical;
        item.consequence = format!(
            "Legacy OpenCode storage has {} session(s) not present in opencode.db. Refusing to touch this directory.",
            unmatched.len()
        );
    } else if item.bytes > 0 {
        item.risk = Risk::Review;
        item.consequence =
            "Legacy JSON session files appear fully migrated into opencode.db. Deleting them only removes leftovers."
                .into();
    }
}

struct SessionStats {
    count: usize,
    oldest_days: u32,
    reclaimable: u64,
}

fn session_stats(db: &Path) -> anyhow::Result<SessionStats> {
    let uri = format!("file:{}?mode=ro", db.display());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let tables = table_names(&conn)?;
    let session_table = pick_table(&tables, &["session", "sessions"]);
    let count: usize = if let Some(table) = session_table {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n.max(0) as usize)
        .unwrap_or(0)
    } else {
        0
    };
    let page_count: u64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as u64;
    let page_size: u64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as u64;
    let freelist: u64 = conn
        .query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as u64;
    let used = page_count
        .saturating_sub(freelist)
        .saturating_mul(page_size);
    Ok(SessionStats {
        count,
        oldest_days: 0,
        reclaimable: used.max(util::disk_usage(db)),
    })
}

fn db_session_ids(db: &Path) -> anyhow::Result<Vec<String>> {
    if !db.exists() {
        return Ok(vec![]);
    }
    let uri = format!("file:{}?mode=ro", db.display());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let tables = table_names(&conn)?;
    let Some(table) = pick_table(&tables, &["session", "sessions"]) else {
        return Ok(vec![]);
    };
    let cols = column_names(&conn, table)?;
    let id_col = if cols.iter().any(|c| c == "id") {
        "id"
    } else if cols.iter().any(|c| c == "session_id") {
        "session_id"
    } else {
        return Ok(vec![]);
    };
    let mut stmt = conn.prepare(&format!("SELECT {id_col} FROM {table}"))?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

fn count_legacy_session_ids(storage: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let walker = jwalk::WalkDir::new(storage)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::Serial);
    for entry in walker.into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.ends_with(".json") {
            ids.push(name.trim_end_matches(".json").to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn table_names(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(names)
}

fn column_names(conn: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(names)
}

fn pick_table<'a>(tables: &'a [String], candidates: &[&str]) -> Option<&'a str> {
    for c in candidates {
        if let Some(t) = tables.iter().find(|t| t.eq_ignore_ascii_case(c)) {
            return Some(t.as_str());
        }
    }
    tables
        .iter()
        .find(|t| t.to_ascii_lowercase().contains("session"))
        .map(|s| s.as_str())
}

pub fn list_session_ids(db: &Path) -> anyhow::Result<Vec<String>> {
    db_session_ids(db)
}

pub fn vacuum_db(db: &Path) -> anyhow::Result<()> {
    let conn = Connection::open(db)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        anyhow::bail!("opencode.db integrity_check failed: {ok}");
    }
    Ok(())
}

pub fn storage_unmatched(data: &Path) -> bool {
    let storage = data.join("storage");
    let db = data.join("opencode.db");
    if !storage.exists() {
        return false;
    }
    let legacy = count_legacy_session_ids(&storage);
    let db_ids = db_session_ids(&db).unwrap_or_default();
    legacy.into_iter().any(|id| !db_ids.contains(&id))
}

#[allow(dead_code)]
fn _fs_exists_hint(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}
