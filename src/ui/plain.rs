use std::io::{self, IsTerminal, Write};

use serde_json::json;

use crate::execute::{ExecReport, Manifest};
use crate::model::{Inventory, Risk};
use crate::optimize::Advice;
use crate::util;

pub fn wants_plain(plain_flag: bool, json_flag: bool) -> bool {
    if json_flag {
        return true;
    }
    if plain_flag {
        return true;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return true;
    }
    !io::stdout().is_terminal()
}

pub fn scan(inv: &Inventory, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(inv)?);
        return Ok(());
    }
    let mut out = io::stdout();
    writeln!(out, "AI Development Storage")?;
    writeln!(out, "─────────────────────────────────────────────")?;
    writeln!(out)?;
    writeln!(
        out,
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Tool", "Total", "Safe", "Review", "User", "Locked"
    )?;
    for tool in &inv.tools {
        if !tool.detected && tool.total_bytes() == 0 {
            continue;
        }
        writeln!(
            out,
            "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10}",
            tool.id,
            util::bytes(tool.total_bytes()),
            util::bytes(tool.bytes_by_risk(Risk::Safe)),
            util::bytes(tool.bytes_by_risk(Risk::Review)),
            util::bytes(tool.bytes_by_risk(Risk::Userdata)),
            util::bytes(tool.bytes_by_risk(Risk::Critical) + tool.bytes_by_risk(Risk::Unknown)),
        )?;
    }
    writeln!(out, "─────────────────────────────────────────────")?;
    writeln!(
        out,
        "{:<12} {:>10} {:>10}",
        "TOTAL",
        util::bytes(inv.total_bytes()),
        util::bytes(inv.bytes_by_risk(Risk::Safe))
    )?;
    writeln!(out)?;

    for tool in &inv.tools {
        if tool.items.is_empty() {
            continue;
        }
        let running = if tool.running { "  (running)" } else { "" };
        let ver = tool.version.as_deref().unwrap_or("");
        writeln!(out, "{} {}{}", tool.id, ver, running)?;
        writeln!(out, "─────────────────────────────────────────")?;
        for item in &tool.items {
            writeln!(
                out,
                "{:<28} {:>10}    {:<10}  {}",
                trunc(&item.label, 28),
                util::bytes(item.bytes),
                item.risk.label(),
                item.rule_id
            )?;
        }
        writeln!(out)?;
    }
    Ok(())
}

pub fn clean_report(report: &ExecReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    let mut out = io::stdout();
    if report.dry_run {
        writeln!(out, "Dry run — nothing deleted.")?;
    } else {
        writeln!(out, "Cleanup complete.")?;
        if let Some(id) = &report.id {
            writeln!(out, "Quarantine id: {id}")?;
        }
    }
    writeln!(out, "Reclaimed (est.): {}", util::bytes(report.bytes))?;
    for p in &report.deleted {
        writeln!(out, "  delete     {}", p.display())?;
    }
    for p in &report.quarantined {
        writeln!(out, "  quarantine {}", p.display())?;
    }
    for s in &report.skipped {
        writeln!(out, "  skip       {s}")?;
    }
    Ok(())
}

pub fn restore_list(list: &[Manifest], json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(list)?);
        return Ok(());
    }
    let mut out = io::stdout();
    if list.is_empty() {
        writeln!(out, "No quarantined snapshots.")?;
        return Ok(());
    }
    writeln!(out, "Quarantine snapshots")?;
    writeln!(out, "─────────────────────────────────────────")?;
    for (i, m) in list.iter().enumerate() {
        writeln!(
            out,
            "[{}] {}  {}  {} item(s)",
            i + 1,
            m.id,
            m.created,
            m.items.len()
        )?;
    }
    writeln!(out, "Use: agentsweep restore --last")?;
    writeln!(out, "  or  agentsweep restore --id <id>")?;
    Ok(())
}

pub fn optimize(advice: &[Advice], json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(advice)?);
        return Ok(());
    }
    let mut out = io::stdout();
    writeln!(out, "Prevention")?;
    writeln!(out, "─────────────────────────────────────────")?;
    for a in advice {
        writeln!(out, "{}", a.tool)?;
        writeln!(out, "  {} = {}", a.key, a.current)?;
        writeln!(out, "  {}", a.note)?;
        writeln!(
            out,
            "  choices: {}",
            a.choices
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        )?;
        writeln!(out, "  recommended: {}", a.recommended)?;
        writeln!(out)?;
    }
    writeln!(out, "Apply recommended: agentsweep optimize --apply")?;
    Ok(())
}

pub fn json_ok(value: impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub fn json_msg(msg: &str) -> anyhow::Result<()> {
    println!("{}", json!({ "ok": true, "message": msg }));
    Ok(())
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{t}…")
}
