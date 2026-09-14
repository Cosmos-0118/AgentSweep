use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use humansize::{format_size, BINARY};

pub fn home_dir() -> PathBuf {
    if let Ok(h) = env::var("HOME") {
        return PathBuf::from(h);
    }
    if let Ok(p) = env::var("USERPROFILE") {
        return PathBuf::from(p);
    }
    PathBuf::from(".")
}

pub fn data_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("agentsweep");
    }
    home_dir().join(".local/share/agentsweep")
}

pub fn quarantine_root() -> PathBuf {
    data_dir().join("quarantine")
}

pub fn bytes(n: u64) -> String {
    format_size(n, BINARY)
}

pub fn age_days(t: SystemTime) -> f64 {
    match SystemTime::now().duration_since(t) {
        Ok(d) => d.as_secs_f64() / 86_400.0,
        Err(_) => 0.0,
    }
}

pub fn is_older_than(t: SystemTime, days: u32) -> bool {
    age_days(t) >= days as f64
}

pub fn disk_usage(path: &Path) -> u64 {
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return 0,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        if meta.is_dir() {
            0
        } else {
            meta.len()
        }
    }
}

pub fn path_mtime(path: &Path) -> Option<SystemTime> {
    path.symlink_metadata().ok().and_then(|m| m.modified().ok())
}

pub fn parse_days(s: &str) -> anyhow::Result<u32> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(n) = s.strip_suffix('d') {
        return Ok(n.parse()?);
    }
    if let Some(n) = s.strip_suffix('h') {
        return Ok((n.parse::<u32>()?).div_ceil(24));
    }
    Ok(s.parse()?)
}

pub fn ignore_name(name: &str) -> bool {
    matches!(
        name,
        ".DS_Store" | ".localized" | "Thumbs.db" | "desktop.ini"
    )
}

pub fn unix_ts(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn first_component(pattern: &str) -> &str {
    pattern
        .split(['/', '\\'])
        .find(|s| !s.is_empty() && *s != ".")
        .unwrap_or(pattern)
}

pub fn has_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}
