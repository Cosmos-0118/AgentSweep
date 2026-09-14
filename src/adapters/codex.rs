use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::{app_support, capture, capture_with_timeout, fallback_home, process_running, Adapter};
use crate::model::{Item, Risk};
use crate::util;

static DOCTOR: OnceLock<Option<Value>> = OnceLock::new();
static VERSION: OnceLock<Option<String>> = OnceLock::new();

pub struct Codex;

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        self.roots().get("home").is_some_and(|p| p.exists())
            || capture("codex", &["--version"]).is_some()
    }

    fn version(&self) -> Option<String> {
        VERSION
            .get_or_init(|| {
                if let Some(json) = doctor_json() {
                    if let Some(v) = json.get("codexVersion").and_then(|v| v.as_str()) {
                        return Some(v.to_string());
                    }
                }
                capture("codex", &["--version"]).map(|s| {
                    s.lines()
                        .find(|l| !l.to_ascii_lowercase().contains("warning"))
                        .unwrap_or(s.trim())
                        .trim()
                        .to_string()
                })
            })
            .clone()
    }

    fn roots(&self) -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        let home = doctor_json()
            .and_then(|j| {
                j.pointer("/checks/config.load/details/CODEX_HOME")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
            })
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| fallback_home(".codex"));
        map.insert("home".into(), home);
        map.insert("desktop".into(), app_support("Codex"));
        map
    }

    fn is_running(&self) -> bool {
        // This used to shell out to `codex doctor --json` for the background
        // app-server daemon's live status, since a daemon can run under a
        // process name a plain process-list check wouldn't recognize. On a
        // real machine that command measured 7-8s wall time, of which the
        // app-server check itself accounted for 0ms - the rest is fixed CLI
        // startup plus two unrelated network round-trips the doctor also
        // runs. There is no timeout short enough to make that probe both
        // fast and useful: anything under ~7s times out every single time,
        // so it was paying its budget on every periodic refresh for a
        // result that structurally never arrives. `process_running` matches
        // on the full command line, not just the process name, so a daemon
        // started as `codex app-server ...` is still caught by the "codex"
        // token in its argv - drop the doctor probe and rely on that alone.
        process_running(&["codex"])
    }

    fn claimed_top_level(&self, root: &str) -> Vec<String> {
        (root == "home")
            .then(|| "packages".into())
            .into_iter()
            .collect()
    }

    fn enrich(&self, items: &mut Vec<Item>) {
        let Some(home) = self.roots().get("home").cloned() else {
            return;
        };
        let (protected, paths, unknown) = match standalone_release_inventory(&home) {
            Some(inventory) => (inventory.protected, inventory.prunable, inventory.unknown),
            None => (vec![], vec![], vec![home.join("packages")]),
        };
        if !unknown.is_empty() {
            items.push(Item {
                rule_id: "codex.packages_unknown".into(),
                tool: self.id().into(),
                label: "Unrecognized Codex package data".into(),
                bytes: unknown
                    .iter()
                    .map(|path| util::disk_usage_recursive(path))
                    .sum(),
                paths: unknown,
                risk: Risk::Unknown,
                requires_stopped: false,
                consequence: "This is outside the validated managed standalone release layout. AgentSweep will not delete it.".into(),
                oldest_mtime: None,
                newest_mtime: None,
                delegate: None,
            });
        }
        if !protected.is_empty() {
            items.push(Item {
                rule_id: "codex.standalone_active_releases".into(),
                tool: self.id().into(),
                label: "Active Codex standalone releases".into(),
                bytes: protected
                    .iter()
                    .map(|path| util::disk_usage_recursive(path))
                    .sum(),
                paths: protected,
                risk: Risk::Critical,
                requires_stopped: false,
                consequence:
                    "The current managed Codex binary and rollback fallback. Never deleted.".into(),
                oldest_mtime: None,
                newest_mtime: None,
                delegate: None,
            });
        }
        if paths.is_empty() {
            return;
        }
        let bytes = paths
            .iter()
            .map(|path| util::disk_usage_recursive(path))
            .sum();
        items.push(Item {
            rule_id: "codex.standalone_releases".into(),
            tool: self.id().into(),
            label: "Superseded Codex releases".into(),
            paths,
            bytes,
            risk: Risk::Safe,
            requires_stopped: true,
            consequence: "Keeps the active standalone release and one fallback release. Deletes older managed Codex binaries only when Codex is closed.".into(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: Some("codex.standalone_prune".into()),
        });
    }
}

/// Codex's standalone updater keeps releases under `releases/` and points
/// `current` at the active one. Only prune after resolving that pointer, and
/// retain the newest non-active directory as a rollback fallback.
pub fn standalone_releases_to_prune(home: &Path) -> Vec<PathBuf> {
    standalone_release_inventory(home)
        .map(|inventory| inventory.prunable)
        .unwrap_or_default()
}

struct StandaloneReleaseInventory {
    protected: Vec<PathBuf>,
    prunable: Vec<PathBuf>,
    unknown: Vec<PathBuf>,
}

fn standalone_release_inventory(home: &Path) -> Option<StandaloneReleaseInventory> {
    let standalone = home.join("packages/standalone");
    let releases = standalone.join("releases");
    let current = standalone.join("current");
    let active = current.canonicalize().ok()?;
    let releases_root = releases.canonicalize().ok()?;
    if active.parent()? != releases_root || release_version(&active).is_none() {
        return None;
    }

    let mut unknown = Vec::new();
    let mut inactive: Vec<(PathBuf, Vec<u64>, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(&releases).ok()?.flatten() {
        let path = entry.path();
        let Some(version) = release_version(&path) else {
            unknown.push(path);
            continue;
        };
        if !entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
            unknown.push(path);
            continue;
        }
        if path.canonicalize().ok()? != active {
            inactive.push((path, version, entry.metadata().ok()?.modified().ok()?));
        }
    }
    inactive.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.0.cmp(&a.0))
    });
    let fallback = inactive.first().map(|(path, _, _)| path.clone());
    let mut protected = vec![active];
    if let Some(fallback) = fallback {
        protected.push(fallback);
    }
    let prunable = inactive
        .into_iter()
        .skip(1)
        .map(|(path, _, _)| path)
        .collect();
    Some(StandaloneReleaseInventory {
        protected,
        prunable,
        unknown,
    })
}

fn release_version(path: &Path) -> Option<Vec<u64>> {
    let name = path.file_name()?.to_str()?;
    let version = name.split_once('-').map_or(name, |(version, _)| version);
    let parts: Vec<u64> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    (parts.len() == 3).then_some(parts)
}

fn doctor_json() -> Option<Value> {
    DOCTOR.get_or_init(doctor_json_fresh).clone()
}

fn doctor_json_fresh() -> Option<Value> {
    doctor_json_with_timeout(Duration::from_secs(3))
}

fn doctor_json_with_timeout(timeout: Duration) -> Option<Value> {
    let stdout = capture_with_timeout("codex", &["doctor", "--json"], timeout)?;
    let trimmed = stdout
        .lines()
        .skip_while(|line| !line.trim_start().starts_with('{'))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&trimmed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn standalone_prune_preserves_current_and_one_fallback() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("agentsweep-codex-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let releases = root.join("packages/standalone/releases");
        for name in ["0.1.0", "0.2.0", "0.3.0"] {
            fs::create_dir_all(releases.join(name)).unwrap();
        }
        symlink(
            releases.join("0.3.0"),
            root.join("packages/standalone/current"),
        )
        .unwrap();

        let prunable = standalone_releases_to_prune(&root);
        assert_eq!(prunable.len(), 1);
        assert_ne!(
            prunable[0].canonicalize().unwrap(),
            releases.join("0.3.0").canonicalize().unwrap()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn standalone_prune_keeps_the_previous_semantic_version_and_surfaces_staging() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("agentsweep-codex-version-{}", std::process::id()));
        let releases = root.join("packages/standalone/releases");
        let _ = fs::remove_dir_all(&root);
        for name in ["0.9.0", "0.10.0", "0.11.0", "staging"] {
            fs::create_dir_all(releases.join(name)).unwrap();
        }
        symlink(
            releases.join("0.11.0"),
            root.join("packages/standalone/current"),
        )
        .unwrap();

        let inventory = standalone_release_inventory(&root).unwrap();
        assert_eq!(inventory.protected.len(), 2);
        assert_eq!(inventory.protected[1], releases.join("0.10.0"));
        assert_eq!(inventory.prunable, vec![releases.join("0.9.0")]);
        assert_eq!(inventory.unknown, vec![releases.join("staging")]);

        let _ = fs::remove_dir_all(&root);
    }
}
