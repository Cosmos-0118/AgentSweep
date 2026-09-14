use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::adapters;
use crate::adapters::opencode;
use crate::model::Item;
use crate::util;

pub fn run(delegate: &str, item: &Item, dry_run: bool) -> anyhow::Result<u64> {
    match delegate {
        "claude.project_purge" => claude_purge(item, dry_run),
        "opencode.session_delete" => opencode_sessions(item, dry_run),
        "opencode.snapshot_prune" => prune_paths(item, dry_run),
        "opencode.legacy_guard" => {
            anyhow::bail!("legacy OpenCode storage is protected")
        }
        other => anyhow::bail!("unknown delegate {other}"),
    }
}

fn claude_purge(item: &Item, dry_run: bool) -> anyhow::Result<u64> {
    if adapters::by_id("claude")
        .map(|a| a.is_running())
        .unwrap_or(false)
    {
        anyhow::bail!("Claude is running");
    }
    let bytes = item.bytes;
    let mut args = vec!["project", "purge", "--all"];
    if dry_run {
        args.push("--dry-run");
    } else {
        args.push("-y");
    }
    let status = Command::new("claude")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(bytes),
        Ok(_) | Err(_) => {
            if dry_run {
                return Ok(bytes);
            }
            // Fallback: do not hand-delete transcripts. Surface the failure.
            anyhow::bail!("claude project purge failed; refusing to delete transcripts manually")
        }
    }
}

fn opencode_sessions(item: &Item, dry_run: bool) -> anyhow::Result<u64> {
    if adapters::by_id("opencode")
        .map(|a| a.is_running())
        .unwrap_or(false)
    {
        anyhow::bail!("OpenCode is running");
    }
    let bytes = item.bytes;
    if dry_run {
        return Ok(bytes);
    }
    let db = item
        .paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("OpenCode session database path is missing"))?;
    // The dashboard's count comes from this database. Do not trust the CLI's
    // formatted list here: it can fail independently or omit rows. The query
    // also applies the 30-day inactivity policy shown in the UI.
    let ids = opencode::stale_session_ids(db)?;
    if ids.is_empty() {
        anyhow::bail!("OpenCode reports no deletable sessions in its database");
    }
    let bytes_before = util::disk_usage(db);
    for id in &ids {
        if adapters::capture("opencode", &["session", "delete", id]).is_none() {
            anyhow::bail!("opencode session delete failed for {id}");
        }
    }
    if adapters::by_id("opencode")
        .map(|a| a.is_running())
        .unwrap_or(false)
    {
        anyhow::bail!("OpenCode started during cleanup; aborting VACUUM");
    }
    // Verify against the complete session table. A session that remains but is
    // touched during cleanup is no longer stale, but it was still not deleted.
    let remaining = opencode::list_session_ids(db)?;
    let undeleted = requested_ids_still_present(&ids, &remaining);
    if !undeleted.is_empty() {
        anyhow::bail!(
            "OpenCode kept {} requested session(s); refusing to report cleanup as successful",
            undeleted.len()
        );
    }
    opencode::vacuum_db(db)?;
    Ok(bytes_before.saturating_sub(util::disk_usage(db)))
}

fn requested_ids_still_present(requested: &[String], remaining: &[String]) -> Vec<String> {
    let remaining: HashSet<_> = remaining.iter().collect();
    requested
        .iter()
        .filter(|id| remaining.contains(id))
        .cloned()
        .collect()
}

fn prune_paths(item: &Item, dry_run: bool) -> anyhow::Result<u64> {
    if adapters::by_id("opencode")
        .map(|a| a.is_running())
        .unwrap_or(false)
    {
        anyhow::bail!("OpenCode is running");
    }
    let bytes = item.bytes;
    if dry_run {
        return Ok(bytes);
    }
    for p in &item.paths {
        remove_path(p)?;
    }
    Ok(bytes)
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn vacuum_copy_check(src: &Path) -> anyhow::Result<u64> {
    let dir = std::env::temp_dir().join(format!("agentsweep-vac-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let dest = dir.join("opencode.db");
    fs::copy(src, &dest)?;
    for suffix in ["-wal", "-shm"] {
        let side_str = format!("{}{suffix}", src.display());
        let side = Path::new(&side_str);
        if side.exists() {
            let _ = fs::copy(side, dir.join(format!("opencode.db{suffix}")));
        }
    }
    let before = util::disk_usage(&dest);
    opencode::vacuum_db(&dest)?;
    let after = util::disk_usage(&dest);
    let _ = fs::remove_dir_all(&dir);
    Ok(before.saturating_sub(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cleanup_requires_every_requested_id_to_disappear() {
        let requested = vec!["ses-a".into(), "ses-b".into()];
        assert_eq!(
            requested_ids_still_present(&requested, &["ses-b".into(), "ses-c".into()]),
            vec!["ses-b"]
        );
        assert!(requested_ids_still_present(&requested, &[]).is_empty());
        // A requested session that became recent still exists and therefore
        // must fail verification; age is irrelevant after deletion starts.
        assert_eq!(
            requested_ids_still_present(&requested, &["ses-a".into()]),
            vec!["ses-a"]
        );
    }
}
