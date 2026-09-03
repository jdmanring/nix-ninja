//! AN EDGE'S SCOPE IS THE FILES THAT REACHED IT, NOT THE ONE IT SITS IN
//! (class 22).
//!
//! Each included file was parsed with a fresh set of variables, so a build
//! statement in a subninja expanded `$cc` to nothing. gyp keeps `cc = ...`
//! in the root and emits a subninja per target, which is why nss failed 513
//! times on `Failed to find -MMD: cannot find binary path` while CMake,
//! whose build statements stay in the root file, was untouched.
//!
//! THE END-TO-END ARM IS `local/gates/argv0-tokenizer-repro.sh`, which
//! drives real builds through the driver and pins the failing shapes too.
//! This file is the seconds-long half: it reads the expanded command out of
//! the graph, so it says WHICH binding was lost rather than that a build
//! failed. It lives in `crates/nix-ninja` for the reason
//! `tests/rpath_assembly.rs` does - the code is inside `nix-ninja-task`'s
//! fileset and a test beside it would re-key every banked plain task.
use std::fs;

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "nn-scope-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

/// Loads `build.ninja` from `dir` and returns every edge's command line.
fn cmdlines(dir: &std::path::Path) -> Vec<String> {
    let cwd = std::env::current_dir().unwrap();
    // n2 resolves `subninja` and `include` against the process's working
    // directory, so the load has to happen inside the tree. The tests that
    // do this run in one binary and one thread each by default; a second
    // test changing directory concurrently would see the wrong tree, which
    // is what the lock below prevents.
    std::env::set_current_dir(dir).unwrap();
    let mut loader = n2::load::Loader::new();
    let mut bytes = fs::read(dir.join("build.ninja")).unwrap();
    bytes.push(0);
    let loaded = loader.parse(std::path::PathBuf::from("build.ninja"), &bytes);
    std::env::set_current_dir(cwd).unwrap();
    loaded.unwrap();
    loader
        .graph
        .builds
        .all_ids()
        .filter_map(|id| loader.graph.builds[id].cmdline.clone())
        .collect()
}

static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_subninja_edge_sees_the_root_binding() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = scratch("sub");
    fs::write(d.join("build.ninja"), "cc = gcc\nsubninja sub.ninja\n").unwrap();
    fs::write(
        d.join("sub.ninja"),
        "rule compile\n  command = $cc -c $in -o $out\nbuild main.o: compile main.c\n",
    )
    .unwrap();

    let cmds = cmdlines(&d);
    assert_eq!(cmds.len(), 1, "expected one edge, got {cmds:?}");
    assert!(
        cmds[0].starts_with("gcc "),
        "the root's binding was lost across the subninja: {:?}",
        cmds[0]
    );
    let _ = fs::remove_dir_all(&d);
}

/// The same defect reached through `include`, with the build statement in
/// the INCLUDED file. CMake puts its rules in an include and its build
/// statements in the root, which is the arrangement that hides this.
#[test]
fn an_included_edge_sees_the_root_binding() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = scratch("inc");
    fs::write(d.join("build.ninja"), "cc = gcc\ninclude rules.ninja\n").unwrap();
    fs::write(
        d.join("rules.ninja"),
        "rule compile\n  command = $cc -c $in -o $out\nbuild main.o: compile main.c\n",
    )
    .unwrap();

    let cmds = cmdlines(&d);
    assert_eq!(cmds.len(), 1, "expected one edge, got {cmds:?}");
    assert!(
        cmds[0].starts_with("gcc "),
        "the root's binding was lost across the include: {:?}",
        cmds[0]
    );
    let _ = fs::remove_dir_all(&d);
}

/// A CHILD REBINDING A NAME WINS INSIDE ITSELF, which is what makes the
/// inherited scope a fallback rather than an override. Without this the
/// forward fix could be written the wrong way round and both tests above
/// would still pass.
#[test]
fn a_child_binding_shadows_the_parents() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = scratch("shadow");
    fs::write(d.join("build.ninja"), "cc = wrongcc\nsubninja sub.ninja\n").unwrap();
    fs::write(
        d.join("sub.ninja"),
        "cc = gcc\nrule compile\n  command = $cc -c $in -o $out\nbuild main.o: compile main.c\n",
    )
    .unwrap();

    let cmds = cmdlines(&d);
    assert!(
        cmds[0].starts_with("gcc "),
        "the parent's binding shadowed the child's: {:?}",
        cmds[0]
    );
    let _ = fs::remove_dir_all(&d);
}

/// A SUBNINJA'S OWN BINDING MUST NOT REACH THE PARENT. Ninja gives a
/// subninja a COPY of the enclosing scope, so its definitions are private;
/// handing the child a shared reference would pass every test above and get
/// this one wrong. `include` shares the parent's scope outright and so
/// SHOULD leak back, which is the deferred half recorded at `parse_with`
/// and pinned as a known gap by the gate rather than here.
#[test]
fn a_subninja_binding_does_not_reach_the_parent() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = scratch("noleak");
    fs::write(
        d.join("build.ninja"),
        "subninja sub.ninja\nbuild main.o: compile main.c\n",
    )
    .unwrap();
    fs::write(
        d.join("sub.ninja"),
        "cc = gcc\nrule compile\n  command = $cc -c $in -o $out\n",
    )
    .unwrap();

    let cmds = cmdlines(&d);
    assert!(
        !cmds[0].starts_with("gcc "),
        "the subninja's binding leaked into the parent: {:?}",
        cmds[0]
    );
    let _ = fs::remove_dir_all(&d);
}
