//! GitHub Copilot Chat sessions for VS Code
//! (`User/globalStorage/github.copilot-chat/session-store.db` plus
//! `emptyWindowChatSessions/*.jsonl`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use super::composer_chat::SESSION_RETENTION_DAYS;
use crate::model::Item;
use crate::util;

pub fn enrich_item(items: &mut [Item], rule_id: &str, app_root: &Path) {
    let Some(item) = items.iter_mut().find(|i| i.rule_id == rule_id) else {
        return;
    };
    let store = session_store(app_root);
    let jsonl_dir = empty_window_sessions(app_root);
    if !store.exists() && !jsonl_dir.is_dir() {
        item.bytes = 0;
        item.delegate = None;
        return;
    }
    match session_stats(&store, &jsonl_dir) {
        Ok(stats) => {
            let mut paths = Vec::new();
            if store.exists() {
                paths.push(store);
            }
            if jsonl_dir.is_dir() {
                paths.push(jsonl_dir);
            }
            item.paths = paths;
            item.bytes = if stats.count == 0 {
                0
            } else {
                stats.reclaimable.max(1)
            };
            item.oldest_mtime = stats.oldest;
            item.newest_mtime = stats.newest;
            item.consequence = format!(
                "VS Code reports {} Copilot Chat session(s) inactive for {}+ days (oldest {} days). Deep clean deletes those rows from session-store.db and matching empty-window transcripts, then VACUUMs. Recent chats stay. This cannot be restored from quarantine.",
                stats.count, SESSION_RETENTION_DAYS, stats.oldest_days
            );
        }
        Err(_) => {
            item.bytes = 0;
            if store.exists() {
                item.paths = vec![store];
            }
            item.consequence = "Could not read VS Code's Copilot Chat session-store.db. Quit VS Code and rescan, or leave this locked.".into();
        }
    }
}

fn session_store(app_root: &Path) -> PathBuf {
    app_root.join("User/globalStorage/github.copilot-chat/session-store.db")
}

fn empty_window_sessions(app_root: &Path) -> PathBuf {
    app_root.join("User/globalStorage/emptyWindowChatSessions")
}

struct SessionStats {
    count: usize,
    oldest_days: u32,
    oldest: Option<SystemTime>,
    newest: Option<SystemTime>,
    reclaimable: u64,
}

fn session_stats(db: &Path, jsonl_dir: &Path) -> anyhow::Result<SessionStats> {
    let mut ids = Vec::new();
    let mut reclaimable = 0u64;
    let mut oldest: Option<SystemTime> = None;
    let mut newest: Option<SystemTime> = None;

    if db.exists() {
        let conn = open_readonly(db)?;
        if has_sessions_table(&conn)? {
            let sessions = stale_sessions(&conn)?;
            reclaimable = reclaimable.saturating_add(estimate_db_bytes(&conn, &sessions)?);
            for (id, updated) in sessions {
                merge_time(&mut oldest, &mut newest, Some(updated));
                ids.push(id);
            }
        }
    }

    // JSONL transcripts for empty-window chats, kept even when absent from DB.
    if jsonl_dir.is_dir() {
        let cutoff = retention_cutoff();
        let Ok(entries) = fs::read_dir(jsonl_dir) else {
            // fall through
            return Ok(finalize(ids, reclaimable, oldest, newest));
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(mt) = util::path_mtime(&path) else {
                continue;
            };
            if mt > cutoff {
                continue;
            }
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            if !ids.iter().any(|existing| existing == &id) {
                ids.push(id);
            }
            reclaimable = reclaimable.saturating_add(util::disk_usage(&path));
            merge_time(&mut oldest, &mut newest, Some(mt));
        }
    }

    Ok(finalize(ids, reclaimable, oldest, newest))
}

fn finalize(
    ids: Vec<String>,
    reclaimable: u64,
    oldest: Option<SystemTime>,
    newest: Option<SystemTime>,
) -> SessionStats {
    let count = ids.len();
    let oldest_days = oldest.map(util::age_days).map(|d| d as u32).unwrap_or(0);
    SessionStats {
        count,
        oldest_days,
        oldest,
        newest,
        reclaimable,
    }
}

fn open_readonly(db: &Path) -> anyhow::Result<Connection> {
    let uri = format!("file:{}?mode=ro", db.display());
    Ok(Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?)
}

fn open_readwrite(db: &Path) -> anyhow::Result<Connection> {
    Ok(Connection::open(db)?)
}

fn has_sessions_table(conn: &Connection) -> anyhow::Result<bool> {
    let mut stmt =
        conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'")?;
    let n: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(n > 0)
}

fn retention_cutoff() -> SystemTime {
    std::time::SystemTime::now()
        - std::time::Duration::from_secs((SESSION_RETENTION_DAYS as u64) * 86_400)
}

fn parse_updated_at(raw: &str) -> Option<SystemTime> {
    // Copilot stores UTC timestamps like 2026-08-05T21:59:49.123Z
    let dt = chrono::DateTime::parse_from_rfc3339(raw).ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.fZ")
            .ok()
            .map(|n| n.and_utc().fixed_offset())
    })?;
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

fn stale_sessions(conn: &Connection) -> anyhow::Result<Vec<(String, SystemTime)>> {
    let cutoff = retention_cutoff();
    let mut stmt = conn
        .prepare("SELECT id, COALESCE(updated_at, created_at) FROM sessions ORDER BY updated_at")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, updated_raw) = row?;
        let Some(updated) = parse_updated_at(&updated_raw) else {
            continue;
        };
        if updated <= cutoff {
            out.push((id, updated));
        }
    }
    Ok(out)
}

fn estimate_db_bytes(conn: &Connection, sessions: &[(String, SystemTime)]) -> anyhow::Result<u64> {
    if sessions.is_empty() {
        return Ok(0);
    }
    let mut total = 0u64;
    for (id, _) in sessions {
        // Approximate: sum text payload lengths for this session's related rows.
        let turns: i64 = conn.query_row(
            "SELECT COALESCE(SUM(length(COALESCE(user_message,'')) + length(COALESCE(assistant_response,''))), 0) \
             FROM turns WHERE session_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        let checkpoints: i64 = conn.query_row(
            "SELECT COALESCE(SUM(length(COALESCE(title,'')) + length(COALESCE(overview,'')) + length(COALESCE(history,''))), 0) \
             FROM checkpoints WHERE session_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        total = total
            .saturating_add(turns as u64)
            .saturating_add(checkpoints as u64)
            .saturating_add(id.len() as u64);
    }
    Ok(total.max(1))
}

fn merge_time(
    oldest: &mut Option<SystemTime>,
    newest: &mut Option<SystemTime>,
    t: Option<SystemTime>,
) {
    let Some(t) = t else { return };
    *oldest = Some(oldest.map_or(t, |o| o.min(t)));
    *newest = Some(newest.map_or(t, |n| n.max(t)));
}

pub fn stale_session_ids(db: &Path) -> anyhow::Result<Vec<String>> {
    if !db.exists() {
        return Ok(vec![]);
    }
    let conn = open_readonly(db)?;
    if !has_sessions_table(&conn)? {
        return Ok(vec![]);
    }
    Ok(stale_sessions(&conn)?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

pub fn list_session_ids(db: &Path) -> anyhow::Result<Vec<String>> {
    if !db.exists() {
        return Ok(vec![]);
    }
    let conn = open_readonly(db)?;
    if !has_sessions_table(&conn)? {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare("SELECT id FROM sessions")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Delete stale Copilot sessions from session-store.db and matching JSONL files.
pub fn delete_stale_sessions(app_root: &Path) -> anyhow::Result<u64> {
    let db = session_store(app_root);
    let jsonl_dir = empty_window_sessions(app_root);
    let mut ids: Vec<String> = if db.exists() {
        stale_session_ids(&db)?
    } else {
        vec![]
    };

    // Include orphan JSONL sessions older than retention.
    if jsonl_dir.is_dir() {
        let cutoff = retention_cutoff();
        if let Ok(entries) = fs::read_dir(&jsonl_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(mt) = util::path_mtime(&path) else {
                    continue;
                };
                if mt > cutoff {
                    continue;
                }
                if let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) {
                    if !ids.iter().any(|e| e == &id) {
                        ids.push(id);
                    }
                }
            }
        }
    }

    if ids.is_empty() {
        anyhow::bail!("VS Code reports no deletable Copilot Chat sessions");
    }

    let before = util::disk_usage(&db).saturating_add(util::disk_usage(&jsonl_dir));

    if db.exists() {
        let conn = open_readwrite(&db)?;
        if !has_sessions_table(&conn)? {
            anyhow::bail!("Copilot Chat sessions table is missing");
        }
        conn.execute_batch("BEGIN IMMEDIATE")?;
        for id in &ids {
            for table in ["turns", "checkpoints", "session_files", "session_refs"] {
                let sql = format!("DELETE FROM {table} WHERE session_id = ?1");
                let _ = conn.execute(&sql, [id]);
            }
            // FTS5 index (best-effort; schema may omit it on older builds).
            let _ = conn.execute("DELETE FROM search_index WHERE session_id = ?1", [id]);
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        }
        conn.execute_batch("COMMIT")?;
        drop(conn);
        vacuum_db(&db)?;
    }

    for id in &ids {
        let jsonl = jsonl_dir.join(format!("{id}.jsonl"));
        if jsonl.exists() {
            let _ = fs::remove_file(&jsonl);
        }
        let debug = app_root
            .join("User/globalStorage/github.copilot-chat/debug-logs")
            .join(id);
        if debug.is_dir() {
            let _ = fs::remove_dir_all(&debug);
        }
    }

    if db.exists() {
        let remaining = list_session_ids(&db)?;
        let leftover: Vec<_> = ids
            .iter()
            .filter(|id| remaining.iter().any(|r| r == *id))
            .cloned()
            .collect();
        if !leftover.is_empty() {
            anyhow::bail!(
                "VS Code kept {} requested Copilot session(s); refusing to report cleanup as successful",
                leftover.len()
            );
        }
    }

    let after = util::disk_usage(&db).saturating_add(util::disk_usage(&jsonl_dir));
    Ok(before.saturating_sub(after))
}

fn vacuum_db(db: &Path) -> anyhow::Result<()> {
    let conn = open_readwrite(db)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        anyhow::bail!("session-store.db integrity_check failed: {ok}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "agentsweep-copilot-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            tag
        ));
        let _ = fs::remove_dir_all(&root);
        let chat = root.join("User/globalStorage/github.copilot-chat");
        let empty = root.join("User/globalStorage/emptyWindowChatSessions");
        fs::create_dir_all(&chat).unwrap();
        fs::create_dir_all(&empty).unwrap();
        let db = chat.join("session-store.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT,
                updated_at TEXT
             );
             CREATE TABLE turns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                user_message TEXT,
                assistant_response TEXT
             );",
        )
        .unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(SESSION_RETENTION_DAYS + 5))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let recent = (chrono::Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
            ("ses-old", old.as_str()),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
            ("ses-recent", recent.as_str()),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO turns (session_id, turn_index, user_message, assistant_response) \
             VALUES (?1, 0, ?2, ?3)",
            ("ses-old", "old question", "old answer"),
        )
        .unwrap();
        drop(conn);
        fs::write(empty.join("ses-old.jsonl"), b"{\"role\":\"user\"}\n").unwrap();
        fs::write(empty.join("ses-recent.jsonl"), b"{\"role\":\"user\"}\n").unwrap();
        root
    }

    #[test]
    fn stale_session_query_excludes_recent() {
        let root = fixture("stale");
        let db = session_store(&root);
        assert_eq!(stale_session_ids(&db).unwrap(), vec!["ses-old"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_removes_old_session_and_jsonl() {
        let root = fixture("delete");
        let freed = delete_stale_sessions(&root).unwrap();
        assert!(list_session_ids(&session_store(&root)).is_ok());
        let _ = freed;
        let db = session_store(&root);
        assert_eq!(list_session_ids(&db).unwrap(), vec!["ses-recent"]);
        assert!(!empty_window_sessions(&root).join("ses-old.jsonl").exists());
        assert!(empty_window_sessions(&root)
            .join("ses-recent.jsonl")
            .exists());
        let _ = fs::remove_dir_all(root);
    }
}
