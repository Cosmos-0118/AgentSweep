use crate::model::Rule;
use anyhow::{Context, Result};
use serde::Deserialize;

const RAW: &str = include_str!("../rules/rules.toml");

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    rule: Vec<Rule>,
}

pub fn load() -> Result<Vec<Rule>> {
    let file: File = toml::from_str(RAW).context("failed to parse embedded rules/rules.toml")?;
    Ok(file.rule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters;
    use crate::model::Risk;

    #[test]
    fn rules_parse_and_contain_codex_logs() {
        let rules = load().unwrap();
        assert!(rules.iter().any(|r| r.id == "codex.logs.sqlite"));
        assert!(rules
            .iter()
            .any(|r| r.risk == Risk::Critical && r.id.contains("auth")));
    }

    #[test]
    fn every_rule_tool_has_a_registered_adapter() {
        let rules = load().unwrap();
        let ids: Vec<&'static str> = adapters::all().iter().map(|a| a.id()).collect();
        for rule in &rules {
            assert!(
                ids.contains(&rule.tool.as_str()),
                "rule {} references unknown tool {}",
                rule.id,
                rule.tool
            );
        }
    }

    #[test]
    fn installed_extensions_are_never_auto_deletable() {
        // The Codex plugins/cache lesson (Concept.md): deleting an installed
        // extension/plugin payload leaves it "installed" but empty. Every
        // rule matching an `extensions` path must stay critical.
        let rules = load().unwrap();
        for rule in &rules {
            if rule.paths.iter().any(|p| p == "extensions") {
                assert_eq!(
                    rule.risk,
                    Risk::Critical,
                    "{} matches an extensions dir but is not critical",
                    rule.id
                );
            }
        }
    }

    #[test]
    fn live_app_state_requires_stopped_regardless_of_risk_tier() {
        // Chromium/Electron cache and webview dirs (Cache, DIPS, Cookies, ...)
        // under an editor's live "app"/"desktop" root, and Windsurf's Cascade
        // working directory (~/.codeium), are actively rewritten by the
        // running app. That's just as true for a Userdata rule as for an
        // auto-cleanable one: `windsurf.codeium.code_tracker` used to skip
        // this, and holding it selected, confirming the delete, and watching
        // it fade out only for the running app to silently recreate the file
        // before the next scan landed - it "reappeared" with no explanation,
        // even though the consequence text promised the history was gone.
        // Every rule here must refuse to run while its tool is open, the
        // same way codex.tmp and opencode.tmp already do.
        let rules = load().unwrap();
        for rule in &rules {
            let root_is_live_app = matches!(rule.root.as_str(), "app" | "desktop")
                || (rule.tool == "windsurf" && rule.root == "codeium");
            let risk_touches_real_files =
                matches!(rule.risk, Risk::Safe | Risk::Review | Risk::Userdata);
            if root_is_live_app && risk_touches_real_files {
                assert!(
                    rule.requires_stopped,
                    "{} touches a live app root and is {:?} but does not require_stopped",
                    rule.id, rule.risk
                );
            }
        }
    }

    #[test]
    fn sqlite_cache_files_include_wal_and_shm_siblings() {
        // A SQLite file deleted without its -wal/-shm siblings (or vice
        // versa) can leave the database inconsistent. Any rule path that
        // names a bare SQLite-looking file must also list its siblings.
        let rules = load().unwrap();
        for rule in &rules {
            for p in &rule.paths {
                if p.ends_with(".sqlite") || *p == "DIPS" || *p == "Cookies" {
                    let wal = format!("{p}-wal");
                    let shm = format!("{p}-shm");
                    let journal = format!("{p}-journal");
                    let has_sibling = rule.paths.iter().any(|q| {
                        q == &wal || q == &shm || q == &journal || q.starts_with(&format!("{p}-"))
                    });
                    assert!(
                        has_sibling,
                        "{} lists {} without a -wal/-shm/-journal sibling",
                        rule.id, p
                    );
                }
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let rules = load().unwrap();
        let mut seen = std::collections::HashSet::new();
        for rule in &rules {
            assert!(
                seen.insert(rule.id.clone()),
                "duplicate rule id {}",
                rule.id
            );
        }
    }
}
