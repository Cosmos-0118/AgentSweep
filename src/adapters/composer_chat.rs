//! Shared Composer/Agent chat cleanup for Cursor-style `state.vscdb`
//! databases (`composerHeaders` + `cursorDiskKV`).
//!
//! Used by Cursor and any VS Code fork that adopts the same schema
//! (Windsurf/Kiro/Antigravity when present).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use crate::model::Item;
use crate::util;

pub const SESSION_RETENTION_DAYS: i64 = 30;

const SESSION_KEY_PREFIXES: &[&str] = &[
    "composerData:",
    "bubbleId:",
    "composerVirtualRowHeights:",
    "checkpointId:",
    "codeBlockPartialInlineDiffFates:",
    "inlineDiff:",
    "ofsContent:",
];

pub fn chat_db(app_root: &Path) -> PathBuf {
    app_root.join("User/globalStorage/state.vscdb")
}

pub struct SessionStats {
    pub count: usize,
    pub oldest_days: u32,
    pub oldest_ms: i64,
    pub newest_ms: i64,
    pub reclaimable: u64,
}

/// Fill a virtual `*.app.chat_sessions` inventory item from a Composer DB.
pub fn enrich_item(items: &mut [Item], rule_id: &str, product: &str, db: &Path) {
    let Some(item) = items.iter_mut().find(|i| i.rule_id == rule_id) else {
        return;
    };
    if !db.exists() {
        item.bytes = 0;
        // No chat database on disk — drop the virtual row (retain keeps
        // delegated 0-byte items otherwise).
        item.delegate = None;
        return;
    }
    match session_stats(db) {
        Ok(stats) => {
            item.paths = vec![db.to_path_buf()];
            item.bytes = if stats.count == 0 {
                0
            } else {
                stats.reclaimable.max(1)
            };
            item.oldest_mtime = millis_to_system_time(stats.oldest_ms);
            item.newest_mtime = millis_to_system_time(stats.newest_ms);
            item.consequence = format!(
                "{product} reports {} Composer/Agent chats inactive for {}+ days (oldest {} days). Deep clean deletes those rows from state.vscdb and VACUUMs. Recent chats and settings stay. This cannot be restored from quarantine.",
                stats.count, SESSION_RETENTION_DAYS, stats.oldest_days
            );
        }
        Err(_) => {
            item.bytes = 0;
            item.paths = vec![db.to_path_buf()];
            item.consequence = format!(
                "Could not read {product}'s chat database (state.vscdb). Quit {product} and rescan, or leave this locked."
            );
        }
    }
}

fn session_stats(db: &Path) -> anyhow::Result<SessionStats> {
    let conn = open_readonly(db)?;
    if !has_composer_tables(&conn)? {
        return Ok(SessionStats {
            count: 0,
            oldest_days: 0,
            oldest_ms: 0,
            newest_ms: 0,
            reclaimable: 0,
        });
    }
    let sessions = stale_sessions(&conn)?;
    let count = sessions.len();
    if count == 0 {
        return Ok(SessionStats {
            count: 0,
            oldest_days: 0,
            oldest_ms: 0,
            newest_ms: 0,
            reclaimable: 0,
        });
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let oldest_ms = sessions.iter().map(|(_, t)| *t).min().unwrap_or(0);
    let newest_ms = sessions.iter().map(|(_, t)| *t).max().unwrap_or(0);
    let oldest_days = ((now_ms - oldest_ms).max(0) / 86_400_000) as u32;
    let ids: Vec<String> = sessions.into_iter().map(|(id, _)| id).collect();
    let reclaimable = estimate_reclaimable(&conn, &ids)?.max(1);
    Ok(SessionStats {
        count,
        oldest_days,
        oldest_ms,
        newest_ms,
        reclaimable,
    })
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

fn has_composer_tables(conn: &Connection) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('composerHeaders', 'cursorDiskKV')",
    )?;
    let n: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(n >= 2)
}

fn retention_cutoff_ms() -> i64 {
    chrono::Utc::now().timestamp_millis() - SESSION_RETENTION_DAYS * 86_400_000
}

fn stale_sessions(conn: &Connection) -> anyhow::Result<Vec<(String, i64)>> {
    let cutoff = retention_cutoff_ms();
    let mut stmt = conn.prepare(
        "SELECT composerId, lastUpdatedAt FROM composerHeaders \
         WHERE lastUpdatedAt IS NOT NULL AND lastUpdatedAt <= ?1 \
         ORDER BY lastUpdatedAt",
    )?;
    let sessions = stmt
        .query_map([cutoff], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sessions)
}

fn estimate_reclaimable(conn: &Connection, ids: &[String]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut total: u64 = 0;

    let mut stmt = conn.prepare(
        "SELECT key, length(value) FROM cursorDiskKV WHERE \
         key LIKE 'composerData:%' OR \
         key LIKE 'bubbleId:%' OR \
         key LIKE 'composerVirtualRowHeights:%' OR \
         key LIKE 'checkpointId:%' OR \
         key LIKE 'codeBlockPartialInlineDiffFates:%' OR \
         key LIKE 'inlineDiff:%' OR \
         key LIKE 'ofsContent:%'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (key, len) = row?;
        if let Some(composer_id) = composer_id_from_kv_key(&key) {
            if id_set.contains(composer_id) {
                total = total.saturating_add(len as u64);
            }
        }
    }

    let mut header_stmt = conn.prepare(
        "SELECT composerId, length(COALESCE(value, '')) + length(composerId) \
         FROM composerHeaders",
    )?;
    let headers = header_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in headers {
        let (id, len) = row?;
        if id_set.contains(id.as_str()) {
            total = total.saturating_add(len as u64);
        }
    }
    Ok(total)
}

fn composer_id_from_kv_key(key: &str) -> Option<&str> {
    for prefix in SESSION_KEY_PREFIXES {
        if let Some(rest) = key.strip_prefix(prefix) {
            return Some(rest.split(':').next().unwrap_or(rest));
        }
    }
    None
}

pub fn stale_session_ids(db: &Path) -> anyhow::Result<Vec<String>> {
    if !db.exists() {
        return Ok(vec![]);
    }
    let conn = open_readonly(db)?;
    if !has_composer_tables(&conn)? {
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
    if !has_composer_tables(&conn)? {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare("SELECT composerId FROM composerHeaders")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

pub fn delete_stale_sessions(db: &Path, product: &str) -> anyhow::Result<u64> {
    let ids = stale_session_ids(db)?;
    if ids.is_empty() {
        anyhow::bail!("{product} reports no deletable chats in its database");
    }
    let before = util::disk_usage(db);
    let conn = open_readwrite(db)?;
    if !has_composer_tables(&conn)? {
        anyhow::bail!("{product} chat tables are missing from state.vscdb");
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    for id in &ids {
        for prefix in SESSION_KEY_PREFIXES {
            if *prefix == "bubbleId:" {
                conn.execute(
                    "DELETE FROM cursorDiskKV WHERE key LIKE ?1",
                    [format!("{prefix}{id}:%")],
                )?;
            } else {
                conn.execute(
                    "DELETE FROM cursorDiskKV WHERE key = ?1",
                    [format!("{prefix}{id}")],
                )?;
                conn.execute(
                    "DELETE FROM cursorDiskKV WHERE key LIKE ?1",
                    [format!("{prefix}{id}:%")],
                )?;
            }
        }
        conn.execute("DELETE FROM composerHeaders WHERE composerId = ?1", [id])?;
    }
    conn.execute_batch("COMMIT")?;
    drop(conn);

    vacuum_db(db)?;

    let remaining = list_session_ids(db)?;
    let leftover: Vec<_> = ids
        .iter()
        .filter(|id| remaining.iter().any(|r| r == *id))
        .cloned()
        .collect();
    if !leftover.is_empty() {
        anyhow::bail!(
            "{product} kept {} requested chat(s); refusing to report cleanup as successful",
            leftover.len()
        );
    }
    Ok(before.saturating_sub(util::disk_usage(db)))
}

pub fn vacuum_db(db: &Path) -> anyhow::Result<()> {
    let conn = open_readwrite(db)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        anyhow::bail!("state.vscdb integrity_check failed: {ok}");
    }
    Ok(())
}

fn millis_to_system_time(ms: i64) -> Option<SystemTime> {
    if ms <= 0 {
        return None;
    }
    let secs = (ms / 1000) as u64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    Some(std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture_db(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let db = std::env::temp_dir().join(format!(
            "agentsweep-composer-{}-{}-{}.db",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            tag
        ));
        let _ = fs::remove_file(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE composerHeaders (
                composerId TEXT PRIMARY KEY,
                lastUpdatedAt INTEGER
             );
             CREATE TABLE cursorDiskKV (
                key TEXT UNIQUE ON CONFLICT REPLACE,
                value BLOB
             );",
        )
        .unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO composerHeaders (composerId, lastUpdatedAt) VALUES (?1, ?2)",
            (
                "chat-old",
                now - (SESSION_RETENTION_DAYS + 5) * 86_400_000,
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO composerHeaders (composerId, lastUpdatedAt) VALUES (?1, ?2)",
            ("chat-recent", now - 86_400_000),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            ("composerData:chat-old", vec![1u8; 1000]),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            ("bubbleId:chat-old:b1", vec![1u8; 500]),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            ("composerData:chat-recent", vec![1u8; 800]),
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn stale_session_query_excludes_recent_chats() {
        let db = fixture_db("stale");
        assert_eq!(stale_session_ids(&db).unwrap(), vec!["chat-old"]);
        let _ = fs::remove_file(db);
    }

    #[test]
    fn delete_stale_sessions_removes_only_old_chat_rows() {
        let db = fixture_db("delete");
        let freed = delete_stale_sessions(&db, "TestIDE").unwrap();
        assert!(freed > 0 || util::disk_usage(&db) > 0);
        assert_eq!(list_session_ids(&db).unwrap(), vec!["chat-recent"]);
        let conn = open_readonly(&db).unwrap();
        let keys: Vec<String> = conn
            .prepare("SELECT key FROM cursorDiskKV ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(keys, vec!["composerData:chat-recent"]);
        let _ = fs::remove_file(db);
    }
}
