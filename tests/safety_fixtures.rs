use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use agentsweep::execute;
use agentsweep::model::{CleanMode, CleanPlan, Item, Risk};
use agentsweep::util;

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
fn manifest_on_disk_accounts_for_every_item_quarantined_so_far() {
    // A worker executing a clean plan can be killed by a crash, a forced
    // quit, or a power loss at any point. The manifest that makes quarantine
    // "recoverable" must reflect every item already moved even if execute()
    // never returns, not only once the whole plan finishes - so it has to be
    // written incrementally rather than once at the end. This can't easily
    // simulate a mid-run kill, but it does pin the on-disk shape those
    // incremental writes must leave behind for every item that succeeds.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup("durable");
    let a = dir.join("a.bin");
    let b = dir.join("b.bin");
    fs::write(&a, vec![1u8; 10]).unwrap();
    fs::write(&b, vec![2u8; 20]).unwrap();

    let plan = CleanPlan::try_new(
        vec![
            item("f.a", Risk::Review, a.clone(), 10),
            item("f.b", Risk::Review, b.clone(), 20),
        ],
        CleanMode::Smart,
        false,
    )
    .unwrap();
    let report = execute::execute(&plan).unwrap();
    let id = report.id.clone().expect("quarantine id must be set");

    let manifest_path = util::quarantine_root().join(&id).join("manifest.json");
    let raw = fs::read_to_string(&manifest_path).unwrap();
    assert!(raw.contains("\"f.a\""), "manifest must list the first item");
    assert!(
        raw.contains("\"f.b\""),
        "manifest must list the second item"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn same_basename_at_different_paths_quarantine_and_restore_distinctly() {
    // A real Windsurf rule lists two candidate paths for the same data - a
    // legacy layout and a current one - and both can exist at once, sharing
    // a basename ("code_tracker"). The two must never collide onto the same
    // quarantine destination: on macOS/APFS a rename onto an
    // already-populated destination from a second source aliases away
    // whatever the first path put there, so the second item silently loses
    // its own recoverable copy.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup("collide");
    let legacy = dir.join("legacy");
    let current = dir.join("nested/current");
    fs::create_dir_all(&legacy).unwrap();
    fs::create_dir_all(&current).unwrap();
    fs::write(legacy.join("code_tracker"), vec![1u8; 10]).unwrap();
    fs::write(current.join("code_tracker"), vec![2u8; 20]).unwrap();

    let plan = CleanPlan::try_new(
        vec![Item {
            rule_id: "windsurf.codeium.code_tracker".into(),
            tool: "windsurf".into(),
            label: "Edit history tracker".into(),
            paths: vec![legacy.join("code_tracker"), current.join("code_tracker")],
            bytes: 30,
            risk: Risk::Userdata,
            requires_stopped: false,
            consequence: String::new(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        }],
        CleanMode::Deep,
        false,
    )
    .unwrap();
    let report = execute::execute(&plan).unwrap();
    assert!(!legacy.join("code_tracker").exists());
    assert!(!current.join("code_tracker").exists());
    assert_eq!(report.quarantined.len(), 2, "both paths must be tracked");

    let n = execute::restore_last().unwrap();
    assert_eq!(n, 2, "both quarantined copies must restore, not just one");
    assert_eq!(
        fs::read(legacy.join("code_tracker")).unwrap(),
        vec![1u8; 10]
    );
    assert_eq!(
        fs::read(current.join("code_tracker")).unwrap(),
        vec![2u8; 20]
    );

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
