use std::io::{self, Write};

use crate::adapters;
use crate::model::{CleanMode, CleanPlan, Item, Risk};
use crate::util;

pub fn running_blockers(items: &[Item]) -> Vec<String> {
    let mut tools: Vec<String> = items
        .iter()
        .filter(|i| i.requires_stopped)
        .map(|i| i.tool.clone())
        .collect();
    tools.sort();
    tools.dedup();
    tools
        .into_iter()
        .filter(|id| adapters::by_id(id).map(|a| a.is_running()).unwrap_or(false))
        .collect()
}

pub fn assert_stopped(items: &[Item]) -> anyhow::Result<()> {
    let blockers = running_blockers(items);
    if blockers.is_empty() {
        return Ok(());
    }
    let mut detail = Vec::new();
    for id in &blockers {
        if let Some(a) = adapters::by_id(id) {
            for proc in a.running_processes() {
                detail.push(proc);
            }
        }
    }
    if detail.is_empty() {
        anyhow::bail!(
            "Refusing to clean while {} is running (background service). Quit it, then retry.",
            blockers.join(", ")
        )
    } else {
        anyhow::bail!(
            "Refusing to clean while {} is running ({}). Quit it, then retry.",
            blockers.join(", "),
            detail.join(", ")
        )
    }
}

pub fn render_refused(item: &Item) -> String {
    format!(
        "\n  🔒   REFUSED\n\n  {}\n  {}\n\n  AgentSweep will never delete auth,\n  configuration, memories, or plugins.\n",
        item.label,
        item.paths
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )
}

pub fn print_refused(item: &Item) {
    let _ = writeln!(io::stderr(), "{}", render_refused(item));
}

pub fn plan_from(items: Vec<Item>, mode: CleanMode, dry_run: bool) -> Result<CleanPlan, Vec<Item>> {
    CleanPlan::try_new(items, mode, dry_run)
}

/// Attempt to force a critical item into a plan. Always fails.
pub fn refuse_critical(item: &Item) -> i32 {
    debug_assert!(item.risk == Risk::Critical || item.risk == Risk::Unknown);
    print_refused(item);
    1
}

pub fn summarize_consequences(items: &[Item]) -> Vec<String> {
    let mut lines: Vec<String> = items
        .iter()
        .filter(|i| i.risk != Risk::Safe)
        .map(|i| format!("{}  —  {}", i.label, util::bytes(i.bytes)))
        .collect();
    lines.sort();
    lines.dedup();
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn critical() -> Item {
        Item {
            rule_id: "codex.auth".into(),
            tool: "codex".into(),
            label: "Credentials".into(),
            paths: vec![PathBuf::from("/tmp/auth.json")],
            bytes: 12,
            risk: Risk::Critical,
            requires_stopped: false,
            consequence: "never".into(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        }
    }

    #[test]
    fn critical_cannot_enter_plan() {
        assert!(plan_from(vec![critical()], CleanMode::Deep, true).is_err());
    }

    #[test]
    fn refuse_critical_exit_code() {
        assert_eq!(refuse_critical(&critical()), 1);
    }
}
