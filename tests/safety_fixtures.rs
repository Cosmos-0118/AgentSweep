use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use agentsweep::execute;
use agentsweep::model::{CleanMode, CleanPlan, Item, Risk};

// Tests in this file redirect quarantine storage via the process-wide
// XDG_DATA_HOME env var. That's global mutable state, so concurrent test
// threads racing on it can read each other's value; serialize them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn item(rule_id: &str, risk: Risk, path: PathBuf, bytes: u64) -> Item {
    Item {
        rule_id: rule_id.into(),
        tool: "fixture".into(),
        label: rule_id.into(),
        paths: vec![path],
        bytes,
        risk,
        requires_stopped: false,
        consequence: String::new(),
        oldest_mtime: None,
        newest_mtime: None,
        delegate: None,
    }
}

fn setup(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agentsweep-it-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Point quarantine at a scratch dir so the real home is untouched.
    unsafe {
        std::env::set_var(
            "XDG_DATA_HOME",
            dir.join("data").to_string_lossy().to_string(),
        );
    }
    dir
}

#[test]
fn safe_item_is_deleted_and_critical_is_refused() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup("safe");
    let target = dir.join("cache.bin");
    fs::write(&target, vec![0u8; 8192]).unwrap();

    let plan = CleanPlan::try_new(
        vec![item("f.cache", Risk::Safe, target.clone(), 8192)],
        CleanMode::Safe,
        false,
    )
    .unwrap();
    let report = execute::execute(&plan).unwrap();
    assert!(!target.exists());
    assert_eq!(report.bytes, 8192);
    assert!(report.deleted.contains(&target));

    // Critical can never enter a plan, even in deep mode.
    let err = CleanPlan::try_new(
        vec![item("f.auth", Risk::Critical, dir.join("auth.json"), 4)],
        CleanMode::Deep,
        false,
    )
    .unwrap_err();
    assert_eq!(err[0].risk, Risk::Critical);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn review_item_is_quarantined_and_restored() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup("quar");
    let target = dir.join("snapshot.bin");
    fs::write(&target, vec![7u8; 4096]).unwrap();

    let plan = CleanPlan::try_new(
        vec![item("f.snap", Risk::Review, target.clone(), 4096)],
        CleanMode::Smart,
        false,
    )
    .unwrap();
    let report = execute::execute(&plan).unwrap();
    assert!(!target.exists(), "original must be moved away");
    assert!(report.id.is_some());
    assert_eq!(report.quarantined, vec![target.clone()]);

    let manifests = execute::list_quarantines().unwrap();
    assert!(manifests.iter().any(|m| Some(m.id.clone()) == report.id));

    let n = execute::restore_last().unwrap();
    assert_eq!(n, 1);
    assert!(target.exists());
    assert_eq!(fs::read(&target).unwrap(), vec![7u8; 4096]);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn userdata_rejected_in_safe_mode() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup("mode");
    let err = CleanPlan::try_new(
        vec![item("f.hist", Risk::Userdata, dir.join("h"), 9)],
        CleanMode::Safe,
        true,
    )
    .unwrap_err();
    assert_eq!(err[0].risk, Risk::Userdata);
    let _ = fs::remove_dir_all(&dir);
}
