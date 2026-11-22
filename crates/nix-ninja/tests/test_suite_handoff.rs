//! A TEST SUITE'S OWN BUILD GOES TO REAL NINJA, ARGUMENTS UNCHANGED.
//!
//! meson-python's tests build meson projects under pytest through whatever
//! `ninja` is on PATH, which under the consumer's drop-in is this driver, and
//! assert on real ninja's failure line and its manifest regeneration. The
//! driver hands such a build to `NN_REAL_NINJA` when `PYTEST_CURRENT_TEST` is
//! set (`cli::test_suite_real_ninja`).

use nix_ninja::cli::{test_suite_real_ninja, HANDED_OFF_VAR};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| OsString::from(v))
    }
}

#[test]
fn hands_off_only_under_pytest_with_a_real_ninja() {
    let none = |p: &[(&str, &str)]| test_suite_real_ninja(env(p)).unwrap();
    assert_eq!(none(&[]), None, "a plain build stays");
    assert_eq!(
        none(&[("NN_REAL_NINJA", "/r/ninja")]),
        None,
        "the drop-in alone is the package's own build"
    );
    assert_eq!(
        none(&[("PYTEST_CURRENT_TEST", "t.py::t (call)")]),
        None,
        "without a real ninja there is nothing to hand to"
    );
    assert_eq!(
        none(&[("PYTEST_CURRENT_TEST", ""), ("NN_REAL_NINJA", "/r/ninja")]),
        None,
        "an empty marker is unset"
    );
    assert_eq!(
        none(&[
            ("PYTEST_CURRENT_TEST", "t.py::t (call)"),
            ("NN_REAL_NINJA", "/r/ninja")
        ]),
        Some("/r/ninja".into())
    );
    let looped = test_suite_real_ninja(env(&[
        ("PYTEST_CURRENT_TEST", "t.py::t (call)"),
        ("NN_REAL_NINJA", "/r/ninja"),
        (HANDED_OFF_VAR, "1"),
    ]));
    assert!(looped.is_err(), "a second hand-off is a loop: {looped:?}");
    let relative = test_suite_real_ninja(env(&[
        ("PYTEST_CURRENT_TEST", "t.py::t (call)"),
        ("NN_REAL_NINJA", "ninja"),
    ]));
    assert!(
        relative.is_err(),
        "a bare name resolves to the drop-in: {relative:?}"
    );
}

const DRIVER: &str = env!("CARGO_BIN_EXE_nix-ninja");

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("nn-handoff-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the driver in `cwd` and fails the test rather than hanging when it
/// does not return: a hand-off that loops never exits on its own.
fn run_bounded(cmd: &mut Command, cwd: &Path) -> Output {
    let mut child = cmd
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the driver did not return within 30 s; an exec loop never does");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A stand-in real ninja: records argv one per line and the hand-off marker,
/// then exits 3 so the caller's status shows the process was replaced.
fn stub(dir: &Path) -> std::path::PathBuf {
    let s = dir.join("ninja");
    let log = dir.join("argv");
    std::fs::write(
        &s,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf 'marker=%s\\n' \"${}\" >> '{}'\nexit 3\n",
            log.display(),
            HANDED_OFF_VAR,
            log.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&s, std::fs::Permissions::from_mode(0o755)).unwrap();
    s
}

#[test]
fn the_binary_execs_real_ninja_with_argv_unchanged() {
    let d = scratch("exec");
    let real = stub(&d);
    let out = run_bounded(
        Command::new(DRIVER)
            .args(["-C", "some dir", "all"])
            .env("PYTEST_CURRENT_TEST", "tests/test_editable.py::t (call)")
            .env("NN_REAL_NINJA", &real)
            .env_remove(HANDED_OFF_VAR),
        &d,
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "real ninja's status is the caller's"
    );
    let argv = std::fs::read_to_string(d.join("argv")).unwrap();
    assert_eq!(argv, "-C\nsome dir\nall\nmarker=1\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.lines().count(),
        1,
        "one line, and it says why: {stderr}"
    );
    assert!(stderr.starts_with("nix-ninja: PYTEST_CURRENT_TEST is set"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_real_ninja_that_leads_back_here_aborts() {
    let d = scratch("loop");
    let out = run_bounded(
        Command::new(DRIVER)
            .arg("all")
            .env("PYTEST_CURRENT_TEST", "t.py::t (call)")
            .env("NN_REAL_NINJA", DRIVER)
            .env_remove(HANDED_OFF_VAR),
        &d,
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("led back to nix-ninja"), "{stderr}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn without_pytest_the_driver_keeps_the_build() {
    let d = scratch("control");
    let real = stub(&d);
    let out = run_bounded(
        Command::new(DRIVER)
            .args(["-C", d.join("absent").to_str().unwrap(), "all"])
            .env_remove("PYTEST_CURRENT_TEST")
            .env("NN_REAL_NINJA", &real),
        &d,
    );
    assert!(!d.join("argv").exists(), "real ninja must not run");
    assert!(
        !out.status.success(),
        "the driver itself ran and found no dir"
    );
    let _ = std::fs::remove_dir_all(&d);
}
