use agentsweep::{execute, model, optimize, safety, scan, ui, util};

use clap::{Parser, Subcommand};

use model::{AgeFilter, CleanMode, CleanPlan, Risk};
use ui::plain;

#[derive(Parser)]
#[command(
    name = "agentsweep",
    version,
    about = "Understand and control what your coding agents store locally"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Disable color and animation
    #[arg(long, global = true)]
    plain: bool,
    /// Machine-readable JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Inventory agent storage
    Scan {
        #[arg(long)]
        tool: Option<String>,
    },
    /// Reclaim disk
    Clean {
        #[arg(long)]
        safe: bool,
        #[arg(long)]
        smart: bool,
        #[arg(long)]
        deep: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        older_than: Option<String>,
        #[arg(long)]
        tool: Option<String>,
    },
    /// Restore quarantined data
    Restore {
        #[arg(long)]
        last: bool,
        #[arg(long)]
        id: Option<String>,
        /// Permanently delete quarantine snapshots older than N days
        #[arg(long, value_name = "DAYS")]
        gc_days: Option<u32>,
    },
    /// Prevention advice
    Optimize {
        #[arg(long)]
        apply: bool,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    let plain = plain::wants_plain(cli.plain, json);

    match cli.command {
        None => {
            if plain {
                let inv = scan::inventory(None)?;
                return plain::scan(&inv, json);
            }
            ui::dash::run()
        }
        Some(Commands::Scan { tool }) => {
            let inv = scan::inventory(tool.as_deref())?;
            plain::scan(&inv, json)
        }
        Some(Commands::Clean {
            safe,
            smart,
            deep,
            dry_run,
            yes,
            older_than,
            tool,
        }) => cmd_clean(safe, smart, deep, dry_run, yes, older_than, tool, json),
        Some(Commands::Restore { last, id, gc_days }) => {
            cmd_restore(last, id, gc_days, json, plain)
        }
        Some(Commands::Optimize { apply }) => cmd_optimize(apply, json),
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_clean(
    safe: bool,
    smart: bool,
    deep: bool,
    mut dry_run: bool,
    yes: bool,
    older_than: Option<String>,
    tool: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let mode = if deep {
        CleanMode::Deep
    } else if smart {
        CleanMode::Smart
    } else {
        let _ = safe;
        CleanMode::Safe
    };
    if matches!(mode, CleanMode::Smart | CleanMode::Deep) && !yes {
        dry_run = true;
    }
    let age = match older_than {
        Some(s) => AgeFilter::Days(util::parse_days(&s)?),
        None => AgeFilter::All,
    };
    let inv = scan::inventory(tool.as_deref())?;
    let items = inv.items_matching(mode, tool.as_deref(), age);

    if items.iter().any(|i| i.risk.locked()) {
        if let Some(item) = items.iter().find(|i| i.risk == Risk::Critical) {
            std::process::exit(safety::refuse_critical(item));
        }
    }

    let plan = match CleanPlan::try_new(items, mode, dry_run) {
        Ok(p) => p,
        Err(forbidden) => {
            if let Some(item) = forbidden.first() {
                std::process::exit(safety::refuse_critical(item));
            }
            anyhow::bail!("nothing to clean");
        }
    };
    if plan.is_empty() {
        eprintln!("Nothing to clean in {} mode.", mode.as_str());
        return Ok(());
    }
    if plan.needs_hold() && !yes && !dry_run {
        anyhow::bail!("userdata/review cleanup requires --yes in non-interactive mode");
    }
    let report = execute::execute(&plan)?;
    plain::clean_report(&report, json)
}

fn cmd_restore(
    last: bool,
    id: Option<String>,
    gc_days: Option<u32>,
    json: bool,
    _plain: bool,
) -> anyhow::Result<()> {
    if let Some(days) = gc_days {
        let freed = execute::purge_expired(days)?;
        if json {
            return plain::json_ok(serde_json::json!({
                "purged_older_than_days": days,
                "freed_bytes": freed,
            }));
        }
        println!(
            "Purged quarantine snapshots older than {days}d, freed {}.",
            util::bytes(freed)
        );
        return Ok(());
    }
    if last {
        let n = execute::restore_last()?;
        if json {
            return plain::json_msg(&format!("restored {n} paths"));
        }
        println!("Restored {n} path(s).");
        return Ok(());
    }
    if let Some(id) = id {
        let n = execute::restore(&id)?;
        if json {
            return plain::json_msg(&format!("restored {n} paths"));
        }
        println!("Restored {n} path(s).");
        return Ok(());
    }
    let list = execute::list_quarantines()?;
    if !json && std::io::IsTerminal::is_terminal(&std::io::stdout()) && !list.is_empty() {
        // Interactive pick-list via TUI would require a full session; print numbered
        // choices and restore --last is the zero-typing default. Humans should use
        // the dashboard `r` key. Scripts pass --id / --last.
        return plain::restore_list(&list, json);
    }
    plain::restore_list(&list, json)
}

fn cmd_optimize(apply: bool, json: bool) -> anyhow::Result<()> {
    if apply {
        let done = optimize::apply_recommended()?;
        if json {
            return plain::json_ok(&done);
        }
        for line in done {
            println!("{line}");
        }
        return Ok(());
    }
    let advice = optimize::collect();
    plain::optimize(&advice, json)
}
