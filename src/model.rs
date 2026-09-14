use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Safe,
    Review,
    Userdata,
    Critical,
    Unknown,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Review => "review",
            Risk::Userdata => "userdata",
            Risk::Critical => "critical",
            Risk::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "SAFE",
            Risk::Review => "REVIEW",
            Risk::Userdata => "USER DATA",
            Risk::Critical => "LOCKED",
            Risk::Unknown => "UNKNOWN",
        }
    }

    pub fn selectable(self, mode: CleanMode) -> bool {
        match (self, mode) {
            (Risk::Safe, _) => true,
            (Risk::Review, CleanMode::Smart | CleanMode::Deep) => true,
            (Risk::Userdata, CleanMode::Deep) => true,
            (Risk::Critical | Risk::Unknown, _) => false,
            _ => false,
        }
    }

    pub fn locked(self) -> bool {
        matches!(self, Risk::Critical | Risk::Unknown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanMode {
    Safe,
    Smart,
    Deep,
}

impl CleanMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanMode::Safe => "safe",
            CleanMode::Smart => "smart",
            CleanMode::Deep => "deep",
        }
    }

    pub fn next(self) -> Self {
        match self {
            CleanMode::Safe => CleanMode::Smart,
            CleanMode::Smart => CleanMode::Deep,
            CleanMode::Deep => CleanMode::Safe,
        }
    }

    pub fn allows(self, risk: Risk) -> bool {
        risk.selectable(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeFilter {
    All,
    Days(u32),
}

impl AgeFilter {
    pub fn cycle(self) -> Self {
        match self {
            AgeFilter::All => AgeFilter::Days(7),
            AgeFilter::Days(7) => AgeFilter::Days(30),
            AgeFilter::Days(30) => AgeFilter::Days(90),
            AgeFilter::Days(_) => AgeFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgeFilter::All => "All",
            AgeFilter::Days(7) => ">7d",
            AgeFilter::Days(30) => ">30d",
            AgeFilter::Days(90) => ">90d",
            AgeFilter::Days(_) => ">Nd",
        }
    }

    pub fn days(self) -> Option<u32> {
        match self {
            AgeFilter::All => None,
            AgeFilter::Days(d) => Some(d),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub id: String,
    pub tool: String,
    pub root: String,
    pub label: String,
    pub paths: Vec<String>,
    pub risk: Risk,
    #[serde(default)]
    pub requires_stopped: bool,
    #[serde(default)]
    pub older_than_days: u32,
    #[serde(default)]
    pub consequence: String,
    #[serde(default)]
    pub delegate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub rule_id: String,
    pub tool: String,
    pub label: String,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub risk: Risk,
    pub requires_stopped: bool,
    pub consequence: String,
    pub oldest_mtime: Option<SystemTime>,
    pub newest_mtime: Option<SystemTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
}

impl Item {
    pub fn passes_age(&self, filter: AgeFilter) -> bool {
        match filter.days() {
            None => true,
            Some(days) => match self.newest_mtime {
                None => true,
                Some(t) => super::util::age_days(t) >= days as f64,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolInventory {
    pub id: String,
    pub version: Option<String>,
    pub detected: bool,
    pub running: bool,
    pub roots: BTreeMap<String, PathBuf>,
    pub items: Vec<Item>,
}

impl ToolInventory {
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    pub fn bytes_by_risk(&self, risk: Risk) -> u64 {
        self.items
            .iter()
            .filter(|i| i.risk == risk)
            .map(|i| i.bytes)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inventory {
    pub tools: Vec<ToolInventory>,
}

impl Inventory {
    pub fn total_bytes(&self) -> u64 {
        self.tools.iter().map(|t| t.total_bytes()).sum()
    }

    pub fn bytes_by_risk(&self, risk: Risk) -> u64 {
        self.tools.iter().map(|t| t.bytes_by_risk(risk)).sum()
    }

    /// Whether two scans show the same reclaimable picture, ignoring fields
    /// that legitimately churn on every scan even when nothing a user would
    /// care about changed: mtimes inside a cache an open editor keeps
    /// touching, and the live `running` process probe. Backing off periodic
    /// rescans on full struct equality would never fire while any tool is
    /// open, which is the common case - so this is the comparison the
    /// refresh backoff should use instead.
    pub fn same_reclaimable_shape(&self, other: &Inventory) -> bool {
        fn fingerprint(inv: &Inventory) -> Vec<(&str, &str, u64)> {
            let mut v: Vec<_> = inv
                .tools
                .iter()
                .flat_map(|t| {
                    t.items
                        .iter()
                        .map(move |i| (t.id.as_str(), i.rule_id.as_str(), i.bytes))
                })
                .collect();
            v.sort();
            v
        }
        fingerprint(self) == fingerprint(other)
    }

    pub fn filtered<'a>(
        &'a self,
        tool: Option<&'a str>,
    ) -> impl Iterator<Item = &'a ToolInventory> {
        self.tools
            .iter()
            .filter(move |t| tool.map(|id| t.id == id).unwrap_or(true))
    }

    pub fn items_matching(&self, mode: CleanMode, tool: Option<&str>, age: AgeFilter) -> Vec<Item> {
        self.filtered(tool)
            .flat_map(|t| t.items.iter())
            .filter(|i| mode.allows(i.risk) && i.passes_age(age) && i.bytes > 0)
            .cloned()
            .collect()
    }
}

/// A clean plan can never contain critical or unknown items.
/// Construction is the type-level gate: the only public constructor rejects them.
#[derive(Debug, Clone)]
pub struct CleanPlan {
    items: Vec<Item>,
    pub mode: CleanMode,
    pub dry_run: bool,
}

impl CleanPlan {
    pub fn try_new(items: Vec<Item>, mode: CleanMode, dry_run: bool) -> Result<Self, Vec<Item>> {
        let forbidden: Vec<Item> = items
            .iter()
            .filter(|i| i.risk.locked() || !mode.allows(i.risk))
            .cloned()
            .collect();
        if !forbidden.is_empty() {
            return Err(forbidden);
        }
        Ok(Self {
            items,
            mode,
            dry_run,
        })
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn needs_hold(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i.risk, Risk::Review | Risk::Userdata))
    }

    pub fn hold_seconds(&self) -> f64 {
        if self.items.iter().any(|i| i.risk == Risk::Userdata) {
            2.0
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(risk: Risk) -> Item {
        Item {
            rule_id: "t".into(),
            tool: "codex".into(),
            label: "x".into(),
            paths: vec![],
            bytes: 1,
            risk,
            requires_stopped: false,
            consequence: String::new(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        }
    }

    fn inventory_with(bytes: u64, mtime: Option<std::time::SystemTime>, running: bool) -> Inventory {
        let mut i = item(Risk::Safe);
        i.bytes = bytes;
        i.newest_mtime = mtime;
        Inventory {
            tools: vec![ToolInventory {
                id: "codex".into(),
                version: None,
                detected: true,
                running,
                roots: std::collections::BTreeMap::new(),
                items: vec![i],
            }],
        }
    }

    #[test]
    fn same_reclaimable_shape_ignores_mtime_and_running_churn() {
        let now = std::time::SystemTime::now();
        let later = now + std::time::Duration::from_secs(10);
        let a = inventory_with(4096, Some(now), false);
        let b = inventory_with(4096, Some(later), true);
        assert!(
            a.same_reclaimable_shape(&b),
            "mtime ticking and a process starting up must not look like a change"
        );
    }

    #[test]
    fn same_reclaimable_shape_detects_a_real_byte_change() {
        let a = inventory_with(4096, None, false);
        let b = inventory_with(8192, None, false);
        assert!(
            !a.same_reclaimable_shape(&b),
            "a changed reclaimable size must be detected"
        );
    }

    #[test]
    fn clean_plan_rejects_critical() {
        let err =
            CleanPlan::try_new(vec![item(Risk::Critical)], CleanMode::Deep, true).unwrap_err();
        assert_eq!(err[0].risk, Risk::Critical);
    }

    #[test]
    fn clean_plan_rejects_unknown() {
        assert!(CleanPlan::try_new(vec![item(Risk::Unknown)], CleanMode::Deep, true).is_err());
    }

    #[test]
    fn clean_plan_rejects_userdata_in_safe_mode() {
        assert!(CleanPlan::try_new(vec![item(Risk::Userdata)], CleanMode::Safe, true).is_err());
    }

    #[test]
    fn clean_plan_accepts_safe() {
        assert!(CleanPlan::try_new(vec![item(Risk::Safe)], CleanMode::Safe, true).is_ok());
    }
}
