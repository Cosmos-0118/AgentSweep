use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::adapters;
use crate::model::{CleanPlan, Item, Risk};
use crate::safety;
use crate::util;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub created: String,
    pub items: Vec<ManifestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestItem {
    pub rule_id: String,
    pub tool: String,
    pub label: String,
    pub original: PathBuf,
    pub quarantined: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecReport {
    pub dry_run: bool,
    pub id: Option<String>,
    pub deleted: Vec<PathBuf>,
    pub quarantined: Vec<PathBuf>,
    pub skipped: Vec<String>,
    /// Rule IDs skipped before their cleanup action began. This lets the
    /// dashboard distinguish a deliberate safety skip from a delete that
    /// reported success but later reappeared.
    pub skipped_rule_ids: Vec<String>,
    pub bytes: u64,
}

pub fn execute(plan: &CleanPlan) -> anyhow::Result<ExecReport> {
    let blockers = safety::running_blockers(plan.items());
    execute_with_blockers(plan, &blockers)
}

/// Execute every item that is safe to touch at the point the cleanup starts.
/// A stopped-tool requirement is deliberately per item rather than a
/// transaction-wide lock: a running Codex must not prevent an unrelated,
/// safe Cursor cache from being cleaned in the same selection.
fn execute_with_blockers(plan: &CleanPlan, blockers: &[String]) -> anyhow::Result<ExecReport> {
    if plan.items().iter().any(|i| i.risk.locked()) {
        panic!("CleanPlan invariant violated: locked item leaked into execute");
    }
    let is_blocked =
        |item: &Item| item.requires_stopped && blockers.iter().any(|tool| tool == &item.tool);

    let mut report = ExecReport {
        dry_run: plan.dry_run,
        id: None,
        deleted: vec![],
        quarantined: vec![],
        skipped: vec![],
        skipped_rule_ids: vec![],
        bytes: 0,
    };

    if plan.dry_run {
        for item in plan.items() {
            if is_blocked(item) {
                report.skipped.push(running_skip(item));
                report.skipped_rule_ids.push(item.rule_id.clone());
                continue;
            }
            report.bytes = report.bytes.saturating_add(item.bytes);
            for p in &item.paths {
                match item.risk {
                    Risk::Safe => report.deleted.push(p.clone()),
                    _ => report.quarantined.push(p.clone()),
                }
            }
        }
        return Ok(report);
    }

    let id = Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let qroot = util::quarantine_root().join(&id);
    let mut manifest = Manifest {
        id: id.clone(),
        created: Local::now().to_rfc3339(),
        items: vec![],
    };
    // Quarantine at least one item as soon as it lands, not only once every
    // item is processed. A crash, kill, or power loss partway through must
    // still leave a manifest that accounts for whatever was already moved -
    // that's the only thing that makes "recoverable from quarantine" true.
    let will_quarantine = plan.items().iter().any(|i| {
        !is_blocked(i) && i.delegate.is_none() && matches!(i.risk, Risk::Review | Risk::Userdata)
    });
    if will_quarantine {
        fs::create_dir_all(&qroot)?;
        write_manifest(&qroot, &manifest)?;
        report.id = Some(id.clone());
    }

    for item in plan.items() {
        if is_blocked(item) {
            report.skipped.push(running_skip(item));
            report.skipped_rule_ids.push(item.rule_id.clone());
            continue;
        }
        if let Some(delegate) = &item.delegate {
            match crate::deep::run(delegate, item, plan.dry_run) {
                Ok(n) => report.bytes += n,
                Err(e) => {
                    report.skipped.push(format!("{}: {e}", item.label));
                    report.skipped_rule_ids.push(item.rule_id.clone());
                }
            }
            continue;
        }
        match item.risk {
            Risk::Safe => {
                for p in &item.paths {
                    let n = util::disk_usage_recursive(p);
                    remove_path(p)?;
                    report.deleted.push(p.clone());
                    report.bytes += n;
                }
            }
            Risk::Review | Risk::Userdata => {
                for p in &item.paths {
                    if !p.exists() {
                        continue;
                    }
                    let n = util::disk_usage_recursive(p);
                    let dest = quarantine_dest(&qroot, item, p);
                    move_path(p, &dest)?;
                    manifest.items.push(ManifestItem {
                        rule_id: item.rule_id.clone(),
                        tool: item.tool.clone(),
                        label: item.label.clone(),
                        original: p.clone(),
                        quarantined: dest.clone(),
                    });
                    // Rewrite after every move, not once at the end: the file on
                    // disk always reflects exactly what has actually been moved
                    // so far, so a restore is never missing an entry.
                    write_manifest(&qroot, &manifest)?;
                    report.quarantined.push(p.clone());
                    report.bytes += n;
                }
            }
            Risk::Critical | Risk::Unknown => unreachable!("locked items cannot be in a CleanPlan"),
        }
    }

    Ok(report)
}

fn running_skip(item: &Item) -> String {
    format!(
        "{}: skipped because {} is running; quit it to clean this item",
        item.label, item.tool
    )
}

fn write_manifest(qroot: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    fs::write(
        qroot.join("manifest.json"),
        serde_json::to_vec_pretty(manifest)?,
    )?;
    Ok(())
}

/// A rule can list more than one candidate path for the same data (e.g. a
/// legacy layout and a current one), and those candidates can share a
/// basename. Naively joining just the basename would then point two
/// different sources at the same destination: on macOS/APFS a rename onto an
/// already-occupied destination silently replaces or nests into it depending
/// on what's there, either way aliasing away the first item's quarantine
/// slot. Check for that collision and disambiguate instead. Paths within one
/// item are quarantined in order, so by the time a later path is checked
/// here, any earlier destination it could collide with already exists on
/// disk - no bookkeeping beyond the filesystem itself is needed.
fn quarantine_dest(qroot: &Path, item: &Item, original: &Path) -> PathBuf {
    let name = original
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "item".into());
    let base = qroot.join(&item.tool).join(&item.rule_id);
    let mut dest = base.join(&name);
    let mut suffix = 2;
    while dest.exists() {
        dest = base.join(format!("{name}-{suffix}"));
        suffix += 1;
    }
    dest
}

fn move_path(src: &Path, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e)
            if e.raw_os_error() == Some(18) || e.kind() == std::io::ErrorKind::CrossesDevices =>
        {
            copy_recursive(src, dest)?;
            remove_path(src)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn copy_recursive(src: &Path, dest: &Path) -> anyhow::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
        Ok(())
    }
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn list_quarantines() -> anyhow::Result<Vec<Manifest>> {
    let root = util::quarantine_root();
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut dirs: Vec<_> = fs::read_dir(&root)?.filter_map(|e| e.ok()).collect();
    dirs.sort_by_key(|e| e.file_name());
    dirs.reverse();
    for entry in dirs {
        let man = entry.path().join("manifest.json");
        if let Ok(bytes) = fs::read(&man) {
            if let Ok(m) = serde_json::from_slice::<Manifest>(&bytes) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

pub fn restore(id: &str) -> anyhow::Result<usize> {
    let root = util::quarantine_root().join(id);
    let man_path = root.join("manifest.json");
    let manifest: Manifest = serde_json::from_slice(&fs::read(&man_path)?)?;
    let mut n = 0;
    for item in &manifest.items {
        if !item.quarantined.exists() {
            continue;
        }
        if let Some(parent) = item.original.parent() {
            fs::create_dir_all(parent)?;
        }
        move_path(&item.quarantined, &item.original)?;
        n += 1;
    }
    let _ = fs::remove_dir_all(&root);
    Ok(n)
}

pub fn restore_last() -> anyhow::Result<usize> {
    let list = list_quarantines()?;
    let Some(first) = list.first() else {
        anyhow::bail!("No quarantined snapshots to restore.");
    };
    restore(&first.id)
}

pub fn purge_expired(days: u32) -> anyhow::Result<u64> {
    let mut freed = 0u64;
    for man in list_quarantines()? {
        let created = chrono::DateTime::parse_from_rfc3339(&man.created)
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc));
        let old = match created {
            Some(t) => (chrono::Utc::now() - t).num_days() >= days as i64,
            None => false,
        };
        if old {
            let dir = util::quarantine_root().join(&man.id);
            freed += dir_size(&dir);
            let _ = fs::remove_dir_all(dir);
        }
    }
    Ok(freed)
}

fn dir_size(path: &Path) -> u64 {
    jwalk::WalkDir::new(path)
        .parallelism(jwalk::Parallelism::Serial)
        .into_iter()
        .flatten()
        .map(|e| util::disk_usage(&e.path()))
        .sum()
}

pub fn adapter_running(id: &str) -> bool {
    adapters::by_id(id).map(|a| a.is_running()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CleanMode, Item, Risk};

    fn item(tool: &str, path: PathBuf, requires_stopped: bool) -> Item {
        Item {
            rule_id: format!("{tool}.cache"),
            tool: tool.into(),
            label: format!("{tool} cache"),
            paths: vec![path],
            bytes: 1,
            risk: Risk::Safe,
            requires_stopped,
            consequence: String::new(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        }
    }

    #[test]
    fn running_tool_items_do_not_block_unrelated_cleanup() {
        let dir = std::env::temp_dir().join(format!(
            "agentsweep-per-tool-blocker-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let codex_target = dir.join("codex-cache.bin");
        let cursor_target = dir.join("cursor-cache.bin");
        fs::write(&codex_target, vec![0u8; 1024]).unwrap();
        fs::write(&cursor_target, vec![0u8; 2048]).unwrap();

        let plan = CleanPlan::try_new(
            vec![
                item("codex", codex_target.clone(), true),
                item("cursor", cursor_target.clone(), false),
            ],
            CleanMode::Safe,
            false,
        )
        .unwrap();
        let report = execute_with_blockers(&plan, &["codex".into()]).unwrap();

        assert!(
            codex_target.exists(),
            "the running tool's item is untouched"
        );
        assert!(
            !cursor_target.exists(),
            "the unrelated item is still cleaned"
        );
        assert!(report
            .skipped
            .iter()
            .any(|skip| skip.contains("codex is running")));
        assert!(report.deleted.contains(&cursor_target));
        let _ = fs::remove_dir_all(&dir);
    }
}
