use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use globset::{Glob, GlobSetBuilder};
use rayon::prelude::*;

use crate::adapters::{self, Adapter};
use crate::model::{Inventory, Item, Risk, Rule, ToolInventory};
use crate::rules;
use crate::util::{self, disk_usage, first_component, has_glob, ignore_name, path_mtime};

pub fn inventory(tool_filter: Option<&str>) -> anyhow::Result<Inventory> {
    let rules = rules::load()?;
    let adapters = adapters::all();
    let tools: Vec<ToolInventory> = adapters
        .par_iter()
        .filter(|a| tool_filter.map(|id| a.id() == id).unwrap_or(true))
        .map(|adapter| scan_tool(adapter.as_ref(), &rules))
        .collect();
    Ok(Inventory { tools })
}

fn scan_tool(adapter: &dyn Adapter, rules: &[Rule]) -> ToolInventory {
    let detected = adapter.detect();
    let roots = adapter.roots();
    // Rules are independent: measure them across threads. Sorting after
    // collect keeps the output deterministic.
    let mut items: Vec<Item> = rules
        .par_iter()
        .filter(|r| r.tool == adapter.id())
        .flat_map_iter(|rule| scan_rule_items(rule, &roots))
        .collect();

    // Unknown sweep is also independent per root: one thread per root.
    let root_list: Vec<(&String, &PathBuf)> = roots.iter().collect();
    let unknowns: Vec<Item> = root_list
        .into_par_iter()
        .filter(|(_, p)| p.exists())
        .flat_map(|(root_key, root_path)| {
            let mut claimed = claimed_top_level(rules, adapter.id(), root_key);
            claimed.extend(adapter.claimed_top_level(root_key));
            unknown_entries(adapter.id(), root_key, root_path, &claimed, &roots)
        })
        .collect();
    items.extend(unknowns);

    adapter.enrich(&mut items);
    items.retain(|i| i.bytes > 0 || i.risk == Risk::Critical || i.delegate.is_some());
    items.sort_by(|a, b| {
        a.risk
            .cmp(&b.risk)
            .then(b.bytes.cmp(&a.bytes))
            .then(a.label.cmp(&b.label))
    });

    ToolInventory {
        id: adapter.id().to_string(),
        version: adapter.version(),
        detected,
        running: adapter.is_running(),
        roots,
        items,
    }
}

fn scan_rule(rule: &Rule, roots: &BTreeMap<String, PathBuf>) -> Item {
    let mut item = Item {
        rule_id: rule.id.clone(),
        tool: rule.tool.clone(),
        label: rule.label.clone(),
        paths: vec![],
        bytes: 0,
        risk: rule.risk,
        requires_stopped: rule.requires_stopped,
        consequence: rule.consequence.clone(),
        oldest_mtime: None,
        newest_mtime: None,
        delegate: rule.delegate.clone(),
    };
    let Some(root) = roots.get(&rule.root) else {
        return item;
    };
    if !root.exists() && rule.paths.iter().all(|p| p != ".") {
        return item;
    }

    let mut matched: Vec<PathBuf> = Vec::new();
    if rule.paths.is_empty() {
        // Virtual item filled by adapter enrich (e.g. OpenCode sessions).
        return item;
    }
    for pattern in &rule.paths {
        matched.extend(resolve(root, pattern));
    }
    matched.sort();
    matched.dedup();
    // A glob can match both a directory and entries below it (for example
    // `logs/**`). Measuring each match recursively would count the same
    // files once for every matching ancestor, and would make cleanup try to
    // remove children after their parent. Keep only the outermost matches.
    let all_matches = matched.clone();
    matched.retain(|candidate| {
        !all_matches
            .iter()
            .any(|other| other != candidate && candidate.starts_with(other))
    });

    let mut bytes = 0u64;
    let mut oldest: Option<SystemTime> = None;
    let mut newest: Option<SystemTime> = None;
    let mut kept: Vec<PathBuf> = Vec::new();

    for path in matched {
        let (size, o, n, files) = measure(&path, rule.older_than_days);
        if size == 0 && files.is_empty() {
            continue;
        }
        bytes += size;
        merge_times(&mut oldest, &mut newest, o, n);
        if rule.older_than_days > 0 {
            kept.extend(files);
        } else {
            kept.push(path);
        }
    }
    item.paths = kept;
    item.bytes = bytes;
    item.oldest_mtime = oldest;
    item.newest_mtime = newest;
    item
}

/// Unknown locations are informational, so presenting several unrelated
/// directories as one item makes its total impossible to attribute to the
/// path shown in the dashboard. Split them into exact locations. Other
/// multi-path rules intentionally model one logical unit (such as a SQLite
/// database plus its WAL/SHM siblings) and remain grouped.
fn scan_rule_items(rule: &Rule, roots: &BTreeMap<String, PathBuf>) -> Vec<Item> {
    if rule.risk != Risk::Unknown || rule.paths.len() <= 1 {
        return vec![scan_rule(rule, roots)];
    }

    rule.paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut location_rule = rule.clone();
            location_rule.id = format!("{}.{}", rule.id, index + 1);
            location_rule.label = format!("{}: {path}", rule.label);
            location_rule.paths = vec![path.clone()];
            scan_rule(&location_rule, roots)
        })
        .collect()
}

fn resolve(root: &Path, pattern: &str) -> Vec<PathBuf> {
    if pattern == "." {
        return vec![root.to_path_buf()];
    }
    if !has_glob(pattern) {
        let p = root.join(pattern);
        if p.exists() {
            return vec![p];
        }
        return vec![];
    }
    let mut builder = GlobSetBuilder::new();
    if let Ok(g) = Glob::new(pattern) {
        builder.add(g);
    }
    // Also match a trailing-slash directory glob.
    if !pattern.ends_with("/**") {
        if let Ok(g) = Glob::new(&format!("{pattern}/**")) {
            builder.add(g);
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    for entry in jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::Serial)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if set.is_match(&rel_s) {
            out.push(path);
        }
    }
    out
}

fn measure(
    path: &Path,
    older_than_days: u32,
) -> (u64, Option<SystemTime>, Option<SystemTime>, Vec<PathBuf>) {
    if !path.exists() {
        return (0, None, None, vec![]);
    }
    if path.is_file()
        || path.is_symlink()
            && path
                .symlink_metadata()
                .map(|m| m.is_file())
                .unwrap_or(false)
    {
        if let Some(mt) = path_mtime(path) {
            if older_than_days > 0 && !util::is_older_than(mt, older_than_days) {
                return (0, None, None, vec![]);
            }
            return (
                disk_usage(path),
                Some(mt),
                Some(mt),
                vec![path.to_path_buf()],
            );
        }
        return (disk_usage(path), None, None, vec![path.to_path_buf()]);
    }

    let mut bytes = 0u64;
    let mut oldest: Option<SystemTime> = None;
    let mut newest: Option<SystemTime> = None;
    let mut files = Vec::new();
    for entry in jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::Serial)
        .into_iter()
        .flatten()
    {
        let p = entry.path();
        // One stat per entry, not two: `path_mtime` and `disk_usage` used to
        // each call `symlink_metadata` independently, doubling syscalls on a
        // walk that is already stat-bound.
        let meta = p.symlink_metadata().ok();
        let mt = meta.as_ref().and_then(util::mtime_from_meta);
        if older_than_days > 0 {
            let Some(mt) = mt else { continue };
            if !util::is_older_than(mt, older_than_days) {
                continue;
            }
            if entry.file_type().is_file() {
                files.push(p.clone());
            }
        }
        bytes += meta.as_ref().map(util::disk_usage_from_meta).unwrap_or(0);
        if let Some(mt) = mt {
            oldest = Some(oldest.map_or(mt, |o| o.min(mt)));
            newest = Some(newest.map_or(mt, |n| n.max(mt)));
        }
    }
    if older_than_days == 0 {
        files.push(path.to_path_buf());
    }
    (bytes, oldest, newest, files)
}

fn merge_times(
    oldest: &mut Option<SystemTime>,
    newest: &mut Option<SystemTime>,
    o: Option<SystemTime>,
    n: Option<SystemTime>,
) {
    if let Some(o) = o {
        *oldest = Some(oldest.map_or(o, |x| x.min(o)));
    }
    if let Some(n) = n {
        *newest = Some(newest.map_or(n, |x| x.max(n)));
    }
}

fn claimed_top_level(rules: &[Rule], tool: &str, root: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for rule in rules.iter().filter(|r| r.tool == tool && r.root == root) {
        for p in &rule.paths {
            if p == "." {
                set.insert(".".into());
                continue;
            }
            set.insert(first_component(p).to_string());
        }
    }
    set
}

fn unknown_entries(
    tool: &str,
    root_key: &str,
    root: &Path,
    claimed: &HashSet<String>,
    roots: &BTreeMap<String, PathBuf>,
) -> Vec<Item> {
    if claimed.contains(".") {
        return vec![];
    }
    if root.is_file() {
        return vec![];
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut items = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if ignore_name(&name_s) {
            continue;
        }
        if claimed.contains(name_s.as_ref()) {
            continue;
        }
        let path = entry.path();
        // Skip entries that are themselves another scanned root of the
        // same tool (e.g. opencode's `log/` and `repos/` live inside `data/`
        // but are scanned via their own roots). Otherwise they'd be
        // double-counted as unknown.
        if roots.values().any(|r| r == &path) {
            continue;
        }
        let (bytes, oldest, newest, _) = measure(&path, 0);
        if bytes == 0 {
            continue;
        }
        items.push(Item {
            rule_id: format!("unknown.{tool}.{root_key}.{name_s}"),
            tool: tool.into(),
            label: format!("Unknown: {name_s}"),
            paths: vec![path],
            bytes,
            risk: Risk::Unknown,
            requires_stopped: false,
            consequence:
                "This path is not in the known storage map. AgentSweep will not delete it.".into(),
            oldest_mtime: oldest,
            newest_mtime: newest,
            delegate: None,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn measures_files_and_unknowns() {
        let dir = std::env::temp_dir().join(format!("agentsweep-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("cache")).unwrap();
        fs::create_dir_all(dir.join("mystery")).unwrap();
        fs::write(dir.join("cache/a"), vec![0u8; 4096]).unwrap();
        fs::write(dir.join("mystery/b"), vec![0u8; 2048]).unwrap();
        let mut f = fs::File::create(dir.join("auth.json")).unwrap();
        f.write_all(b"secret").unwrap();

        let rule = Rule {
            id: "t.cache".into(),
            tool: "t".into(),
            root: "home".into(),
            label: "cache".into(),
            paths: vec!["cache".into()],
            risk: Risk::Safe,
            requires_stopped: false,
            older_than_days: 0,
            consequence: String::new(),
            delegate: None,
        };
        let crit = Rule {
            id: "t.auth".into(),
            tool: "t".into(),
            root: "home".into(),
            label: "auth".into(),
            paths: vec!["auth.json".into()],
            risk: Risk::Critical,
            requires_stopped: false,
            older_than_days: 0,
            consequence: String::new(),
            delegate: None,
        };
        let mut roots = BTreeMap::new();
        roots.insert("home".into(), dir.clone());
        let cache = scan_rule(&rule, &roots);
        assert!(cache.bytes >= 4096);
        let claimed = claimed_top_level(&[rule, crit], "t", "home");
        let unk = unknown_entries("t", "home", &dir, &claimed, &roots);
        assert!(unk.iter().any(|i| i.label.contains("mystery")));
        assert!(unk.iter().all(|i| !i.label.contains("cache")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_multi_location_rules_are_split_into_exact_items() {
        let dir = std::env::temp_dir().join(format!("agentsweep-unknown-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("packages")).unwrap();
        fs::create_dir_all(dir.join("agents")).unwrap();
        fs::write(dir.join("packages/cache.bin"), vec![0u8; 8192]).unwrap();
        fs::write(dir.join("agents/config.toml"), b"small").unwrap();

        let rule = Rule {
            id: "t.unknown".into(),
            tool: "t".into(),
            root: "home".into(),
            label: "Unclassified test data".into(),
            paths: vec!["packages".into(), "agents".into()],
            risk: Risk::Unknown,
            requires_stopped: false,
            older_than_days: 0,
            consequence: String::new(),
            delegate: None,
        };
        let mut roots = BTreeMap::new();
        roots.insert("home".into(), dir.clone());

        let items = scan_rule_items(&rule, &roots);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.paths.len() == 1));
        let packages = items
            .iter()
            .find(|item| item.label.ends_with(": packages"))
            .unwrap();
        let agents = items
            .iter()
            .find(|item| item.label.ends_with(": agents"))
            .unwrap();
        assert_eq!(packages.paths, vec![dir.join("packages")]);
        assert_eq!(agents.paths, vec![dir.join("agents")]);
        assert!(packages.bytes > agents.bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn globbed_parent_and_children_are_measured_once() {
        let dir = std::env::temp_dir().join(format!("agentsweep-glob-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("logs/nested")).unwrap();
        fs::write(dir.join("logs/nested/output.log"), vec![0u8; 8192]).unwrap();

        let rule = Rule {
            id: "t.logs".into(),
            tool: "t".into(),
            root: "home".into(),
            label: "logs".into(),
            paths: vec!["logs/**".into()],
            risk: Risk::Safe,
            requires_stopped: false,
            older_than_days: 0,
            consequence: String::new(),
            delegate: None,
        };
        let mut roots = BTreeMap::new();
        roots.insert("home".into(), dir.clone());
        let item = scan_rule(&rule, &roots);

        assert_eq!(item.paths, vec![dir.join("logs/nested")]);
        assert_eq!(
            item.bytes,
            util::disk_usage_recursive(&dir.join("logs/nested"))
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
