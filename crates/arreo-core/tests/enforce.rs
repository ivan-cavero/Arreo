//! T-0019 failing-first probes: cgroup guard lifecycle + breach behavior.
//!
//! Written before `src/enforce.rs` exists — MUST fail to compile until it lands.
//! NOTE: these tests touch the LIVE cgroupfs (own scope only, cleaned up by
//! Drop). They self-skip when cgroup v2 is unavailable (CI macOS/Windows,
//! containers without delegation) — skip, never fake.

use arreo_core::enforce::{Budget, Guard};

fn cgroup_v2_live() -> bool {
    // Presence is NOT enough (this dev box exposes controllers but denies
    // writes — no delegation under the harness scope). Probe WRITABILITY:
    // resolve our scope, mkdir a temp group, try a limit write, clean up.
    // CI ubuntu (delegation available, like rootless podman) passes; here we
    // skip honestly instead of failing on environment, not code.
    let scope = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (prefix, rest) = line.split_once(':')?;
                let (_, cpath) = rest.split_once(':')?;
                (prefix == "0").then(|| format!("/sys/fs/cgroup{cpath}"))
            })
        });
    let Some(scope) = scope else { return false };
    let probe = std::path::PathBuf::from(format!("{scope}/arreo-probe-{}", std::process::id()));
    if std::fs::create_dir_all(&probe).is_err() {
        return false;
    }
    let writable = std::fs::write(probe.join("memory.max"), "max").is_ok();
    let _ = std::fs::remove_dir(&probe);
    writable
}

#[test]
fn guard_applies_limits_and_reports() {
    if !cgroup_v2_live() {
        eprintln!("SKIP: no cgroup v2 (not Linux with delegation)");
        return;
    }
    let guard = Guard::create(
        "arreo-test-limits",
        Budget {
            memory_max: Some(256 * 1024 * 1024),
            pids_max: Some(64),
        },
    )
    .expect("guard creates group under own scope");
    assert_eq!(
        guard.memory_max().expect("read back"),
        Some(256 * 1024 * 1024)
    );
    assert_eq!(guard.pids_max().expect("read back"), Some(64));
}

#[test]
fn memory_hog_hits_the_ceiling_not_the_host() {
    if !cgroup_v2_live() {
        eprintln!("SKIP: no cgroup v2 (not Linux with delegation)");
        return;
    }
    // 64 MB ceiling; child tries to eat 512 MB. The OOM killer takes the
    // child (or malloc fails) — either way the HOST stays safe and the
    // guard reports over-limit via memory.events.
    let guard = Guard::create(
        "arreo-test-hog",
        Budget {
            memory_max: Some(64 * 1024 * 1024),
            pids_max: Some(32),
        },
    )
    .expect("guard");
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("python3 -c \"x = bytearray(512*1024*1024); import time; time.sleep(30)\"; sleep 30")
        .spawn()
        .expect("spawn hog");
    guard.attach(child.id()).expect("move hog into group");
    // Wait for the ceiling to bite (events or death), up to 20 s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut breached = false;
    while std::time::Instant::now() < deadline {
        if guard.breached().expect("poll").is_some() {
            breached = true;
            break;
        }
        if child.try_wait().expect("wait").is_some() {
            breached = true; // OOM-killed counts as enforced.
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(breached, "64 MB ceiling bit a 512 MB hog within 20 s");
}

#[test]
fn pids_ceiling_blocks_fork_bombs() {
    if !cgroup_v2_live() {
        eprintln!("SKIP: no cgroup v2 (not Linux with delegation)");
        return;
    }
    let guard = Guard::create(
        "arreo-test-pids",
        Budget {
            memory_max: None,
            pids_max: Some(8),
        },
    )
    .expect("guard");
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("for i in $(seq 1 40); do sleep 30 & done; wait")
        .spawn()
        .expect("spawn forker");
    guard.attach(child.id()).expect("move forker into group");
    std::thread::sleep(std::time::Duration::from_secs(3));
    // Count group members: must be ≤ ceiling (kernel refused the rest).
    let members = guard.member_count().expect("count");
    let _ = child.kill();
    let _ = child.wait();
    assert!(members <= 8, "pids.max held: {members} members");
}

#[test]
fn guard_drop_removes_the_group() {
    if !cgroup_v2_live() {
        eprintln!("SKIP: no cgroup v2 (not Linux with delegation)");
        return;
    }
    let path = {
        let guard = Guard::create(
            "arreo-test-drop",
            Budget {
                memory_max: None,
                pids_max: None,
            },
        )
        .expect("guard");
        guard.path().to_path_buf()
    };
    assert!(!path.exists(), "group removed on Drop: {}", path.display());
}
