use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::adapters;
use crate::util;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Advice {
    pub tool: String,
    pub key: String,
    pub current: String,
    pub recommended: String,
    pub choices: Vec<Choice>,
    pub note: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Choice {
    pub label: String,
    pub value: String,
}

pub fn collect() -> Vec<Advice> {
    let mut out = Vec::new();
    if let Some(a) = codex_advice() {
        out.push(a);
    }
    if let Some(a) = claude_advice() {
        out.push(a);
    }
    if let Some(a) = opencode_advice() {
        out.push(a);
    }
    out
}

fn codex_advice() -> Option<Advice> {
    let home = adapters::Codex.roots().get("home")?.clone();
    let path = home.join("config.toml");
    if !path.exists() {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let current = extract_toml_value(&text, "max_bytes")
        .or_else(|| {
            if text.contains("persistence") && text.contains("none") {
                Some("off".into())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unlimited".into());
    Some(Advice {
        tool: "codex".into(),
        key: "history.max_bytes".into(),
        current: current.clone(),
        recommended: "104857600".into(),
        choices: vec![
            Choice {
                label: "Unlimited".into(),
                value: "unlimited".into(),
            },
            Choice {
                label: "500 MB".into(),
                value: "524288000".into(),
            },
            Choice {
                label: "100 MB".into(),
                value: "104857600".into(),
            },
            Choice {
                label: "Off".into(),
                value: "off".into(),
            },
        ],
        note: "Codex prompt history has no size cap unless you set history.max_bytes.".into(),
        path,
    })
}

fn claude_advice() -> Option<Advice> {
    let home = adapters::Claude.roots().get("home")?.clone();
    let path = home.join("settings.json");
    let current: String = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("cleanupPeriodDays")
                .and_then(|n| n.as_u64())
                .map(|n| n.to_string())
        })
        .unwrap_or_else(|| "30".into());
    Some(Advice {
        tool: "claude".into(),
        key: "cleanupPeriodDays".into(),
        current: current.clone(),
        recommended: "30".into(),
        choices: vec![
            Choice {
                label: "7 days".into(),
                value: "7".into(),
            },
            Choice {
                label: "30 days".into(),
                value: "30".into(),
            },
            Choice {
                label: "90 days".into(),
                value: "90".into(),
            },
        ],
        note: "Claude auto-deletes eligible session files after this many days.".into(),
        path,
    })
}

fn opencode_advice() -> Option<Advice> {
    let config = adapters::OpenCode
        .roots()
        .get("config")
        .cloned()
        .unwrap_or_else(|| util::home_dir().join(".config/opencode"));
    let path = ["opencode.json", "opencode.jsonc"]
        .iter()
        .map(|n| config.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| config.join("opencode.json"));
    let current: String = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("snapshots").cloned())
        .map(|v| match v {
            Value::Bool(false) => "off".into(),
            _ => "on".into(),
        })
        .unwrap_or_else(|| "on".into());
    Some(Advice {
        tool: "opencode".into(),
        key: "snapshots".into(),
        current: current.clone(),
        recommended: "on".into(),
        choices: vec![
            Choice {
                label: "On".into(),
                value: "on".into(),
            },
            Choice {
                label: "Off".into(),
                value: "off".into(),
            },
        ],
        note: "Snapshots enable /undo. Turning them off stops future capture; existing snapshots stay until cleaned.".into(),
        path,
    })
}

pub fn apply(advice: &Advice, value: &str) -> anyhow::Result<()> {
    backup(&advice.path)?;
    match advice.tool.as_str() {
        "codex" => apply_codex(&advice.path, value),
        "claude" => apply_claude(&advice.path, value),
        "opencode" => apply_opencode(&advice.path, value),
        other => anyhow::bail!("unknown tool {other}"),
    }
}

pub fn apply_recommended() -> anyhow::Result<Vec<String>> {
    let mut done = Vec::new();
    for a in collect() {
        apply(&a, &a.recommended)?;
        done.push(format!("{} {} -> {}", a.tool, a.key, a.recommended));
    }
    Ok(done)
}

fn backup(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        let bak = path.with_extension(format!(
            "{}.bak.{}",
            path.extension().and_then(|s| s.to_str()).unwrap_or("bak"),
            chrono::Local::now().format("%Y%m%d%H%M%S")
        ));
        fs::copy(path, bak)?;
    }
    Ok(())
}

fn apply_codex(path: &Path, value: &str) -> anyhow::Result<()> {
    let mut text = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    if value == "off" {
        upsert_toml_section(&mut text, "history", "persistence", "\"none\"");
    } else if value == "unlimited" {
        upsert_toml_section(&mut text, "history", "persistence", "\"save-all\"");
        // leave max_bytes unset / remove it
        remove_toml_key(&mut text, "max_bytes");
    } else {
        upsert_toml_section(&mut text, "history", "persistence", "\"save-all\"");
        upsert_toml_section(&mut text, "history", "max_bytes", value);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

fn apply_claude(path: &Path, value: &str) -> anyhow::Result<()> {
    let mut v: Value = if path.exists() {
        serde_json::from_str(&fs::read_to_string(path)?)?
    } else {
        json!({})
    };
    let days: u64 = value.parse()?;
    v["cleanupPeriodDays"] = json!(days);
    fs::write(path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

fn apply_opencode(path: &Path, value: &str) -> anyhow::Result<()> {
    let mut v: Value = if path.exists() {
        serde_json::from_str(&fs::read_to_string(path).unwrap_or_else(|_| "{}".into()))
            .unwrap_or(json!({}))
    } else {
        json!({})
    };
    v["snapshots"] = json!(value != "off");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

fn extract_toml_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(v) = rest.trim().strip_prefix('=') {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn upsert_toml_section(text: &mut String, section: &str, key: &str, value: &str) {
    let header = format!("[{section}]");
    if !text.contains(&header) {
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("\n{header}\n{key} = {value}\n"));
        return;
    }
    let mut found = false;
    let mut in_section = false;
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !found {
                lines.push(format!("{key} = {value}"));
                found = true;
            }
            in_section = trimmed == header;
        }
        if in_section && trimmed.starts_with(key) && trimmed.contains('=') {
            lines.push(format!("{key} = {value}"));
            found = true;
            continue;
        }
        lines.push(line.to_string());
    }
    if in_section && !found {
        lines.push(format!("{key} = {value}"));
    }
    *text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
}

fn remove_toml_key(text: &mut String, key: &str) {
    let lines: Vec<String> = text
        .lines()
        .filter(|line| {
            let t = line.trim();
            !(t.starts_with(key) && t.contains('='))
        })
        .map(|s| s.to_string())
        .collect();
    *text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
}

impl adapters::Claude {
    pub fn roots(&self) -> std::collections::BTreeMap<String, PathBuf> {
        adapters::Adapter::roots(self)
    }
}

impl adapters::Codex {
    pub fn roots(&self) -> std::collections::BTreeMap<String, PathBuf> {
        adapters::Adapter::roots(self)
    }
}

impl adapters::OpenCode {
    pub fn roots(&self) -> std::collections::BTreeMap<String, PathBuf> {
        adapters::Adapter::roots(self)
    }
}
