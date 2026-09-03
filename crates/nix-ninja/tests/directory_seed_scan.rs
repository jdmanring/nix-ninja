//! A DIRECTORY IN THE SCAN'S SEED SET FAILS THE WHOLE TASK (class 21).
//!
//! `retrieve_c_includes_checked` walks every path it is handed and reads it.
//! A derived file whose `rel_path` is itself a DIRECTORY reaches that walk
//! through result handling, and the read is EISDIR:
//!
//!     Failed to read file <store>/ninja-build-include-llvm/include/llvm:
//!       Is a directory (os error 21)
//!
//! 185 task failures across llvm-tblgen and rdma-core in one round, and
//! every edition needs clang.
//!
//! WHAT THESE TESTS ESTABLISH IS A BOUND, NOT A CLOSURE. Both routes a
//! reduction can construct - a directory in the SEED SET, and a directory
//! reached through the VIRTUAL PATH map - are refused at HEAD, and each is
//! refused with `Required file not found` rather than the round's EISDIR.
//! So neither ALONE is the round's route. THE ROUND'S ROUTE IS THE PAIRING,
//! and the third test below constructs it: a directory that is a seed AND a
//! virtual key at once. That answer arrived after the first two tests were
//! written, and the two of them are what bounded where it was not.
//! `4e591a0` added the directory guard on 2026-08-29, 183 commits before
//! this file, and it is an ancestor of the reporting round's pin - so the
//! guard was present and the failures happened anyway.
//!
//! THIS FILE LIVES IN `crates/nix-ninja` DELIBERATELY. The scanner is in
//! `crates/deps-infer`, inside `nix-ninja-task`'s fileset, so the same test
//! written beside the code would re-key every banked PLAIN task derivation
//! to buy coverage that never runs inside a build. It reaches the same
//! `pub` function from outside the allowlist, which is the worked example
//! `crates/nix-ninja/tests/rpath_assembly.rs` already sets.
use std::fs;

/// A directory in the seed set is REFUSED before the read, and the message
/// is the discriminator: `Required file not found` and never EISDIR. That
/// distinction is the whole value of this test, because a session reading
/// class 21's error text would otherwise write a fix for this route.
#[test]
fn a_directory_in_the_seed_set_is_refused_before_the_read() {
    let d = std::env::temp_dir().join(format!(
        "nn-dirseed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(d.join("include/llvm")).unwrap();
    fs::write(d.join("tu.c"), b"int main(void){return 0;}\n").unwrap();

    // A REAL FILE ALONE SCANS CLEAN, which is the control: without it a
    // failure here would say nothing about the directory.
    let ok = deps_infer::c_include_parser::retrieve_c_includes_checked(
        "gcc -c tu.c -o tu.o",
        vec![d.join("tu.c")],
        None,
    );
    // Same reason as below: `Scan` is not Debug, so the control reports its
    // error text rather than the whole Result.
    if let Err(e) = ok {
        panic!("a plain source must scan, got: {e:#}");
    }

    let got = deps_infer::c_include_parser::retrieve_c_includes_checked(
        "gcc -c tu.c -o tu.o",
        vec![d.join("tu.c"), d.join("include/llvm")],
        None,
    );
    // `Scan` is not Debug, so `expect_err` cannot be used on this Result.
    let err = match got {
        Ok(_) => panic!("a directory in the seed set must not scan clean"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        err.contains("Required file not found"),
        "the guard must refuse it by resolution, got: {err}"
    );
    assert!(
        !err.contains("os error 21"),
        "reaching the read means the guard was bypassed: {err}"
    );

    let _ = fs::remove_dir_all(&d);
}

/// THE ROUTE THAT LOOKED LIKE THE ROUND'S AND IS NOT.
///
/// `canonicalize_cached` answers a VIRTUAL PATH hit before it ever reaches
/// the `is_dir` guard added for webrtc-audio-processing, and the virtual map
/// is the materialized-output map that "grows with every materialized
/// output". So a derived file whose `rel_path` is a DIRECTORY is handed back
/// as a resolved include, the BFS queues it, and `scan_directives` reads it.
///
/// Reading the source says that short-circuit is upstream of the guard, so
/// a materialized directory should sail past it. IT DOES NOT: the resolved
/// value is canonicalized again downstream and refused there. Reading the
/// order of two branches is not a claim about what the walk does with the
/// value afterwards, and this test is what turned that reading into a
/// measurement.
///
/// AND IT IS ALSO THE POST-FIX RESIDUAL, MEASURED RATHER THAN ARGUED, which
/// is not what it was written for. Its configuration is exactly what
/// `scan_seeds` leaves behind: the directory is NOT a seed here, its virtual
/// entry IS present, and a source includes it by name. So this is what a
/// task looks like after the fix if any source reaches that directory
/// directly - and the refusal it asserts is `?`-propagated, so the task
/// dies. The residual was described as reasoning when a passing test had
/// already measured it.
#[test]
fn a_virtual_path_resolving_to_a_directory_is_also_refused() {
    let d = std::env::temp_dir().join(format!(
        "nn-dirvirt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store_dirlike = d.join("ninja-build-include-llvm/include/llvm");
    fs::create_dir_all(&store_dirlike).unwrap();
    fs::create_dir_all(d.join("src")).unwrap();
    let tu = d.join("src/tu.c");
    fs::write(&tu, b"#include \"llvm\"\nint main(void){return 0;}\n").unwrap();

    let mut virtual_paths = std::collections::HashMap::new();
    virtual_paths.insert(d.join("src/llvm"), store_dirlike.clone());

    let got = deps_infer::c_include_parser::retrieve_c_includes_checked(
        &format!("gcc -c {} -o tu.o", tu.display()),
        vec![tu.clone()],
        Some(virtual_paths),
    );
    match got {
        Ok(_) => panic!("a directory must not scan clean through the virtual map"),
        Err(e) => {
            let err = format!("{e:#}");
            assert!(
                err.contains("Required file not found"),
                "expected refusal by resolution, got: {err}"
            );
            assert!(
                !err.contains("os error 21"),
                "reaching the read means class 21's route is live here: {err}"
            );
        }
    }

    let _ = fs::remove_dir_all(&d);
}

/// THE ROUND'S ROUTE, and it is the COMBINATION the two tests above miss.
///
/// `discover_dynamic_dependencies` passes its seed set and its virtual map
/// from ONE map, `built_paths` (`subtool/dynamic_task.rs`; named rather
/// than cited by line, because the commit that fixes this moves those
/// lines). So a directory-shaped built
/// input is a seed that is ALSO a virtual key: `canonicalize_cached` takes
/// the virtual hit and returns the store path without ever reaching its own
/// `is_dir` guard further down, the walk hands that store path
/// to `scan_directives`, and the read is EISDIR.
///
/// Neither arm above reproduces it, and that is not redundancy: one has a
/// directory seed with NO virtual entry, the other a virtual directory
/// reached as an INCLUDE. Each is refused by a different downstream check.
/// It is the pairing that is unguarded.
///
///     rdma-core       161 occurrences, include/kernel-abi
///     llvm-tblgen      28 occurrences, include/llvm
///
/// both at pin 6015510, which has the guard: `4e591a0` is its ancestor.
///
/// THIS ARM ASSERTS THE DEFECT AND IS POINTED AT THE ROOT CAUSE, NOT AT THE
/// FIX. `scan_seeds` stops the driver from SEEDING a directory; it does not
/// stop `canonicalize_cached` handing one back, which is what this
/// exercises. So this stays green with the fix in, and it goes RED the day
/// someone guards the virtual hit itself - the two-line change in
/// `crates/deps-infer` that is declined today on cost.
/// IF THIS FAILS, READ IT AS THE ROOT CAUSE HAVING BEEN FIXED and delete
/// the arm deliberately. Do not repair it. An arm asserting a live defect
/// is the only thing that makes such a fix impossible to land silently,
/// and the failure mode of the pattern is a later author quietly deleting
/// a test they read as broken.
#[test]
fn a_directory_that_is_both_seed_and_virtual_key_reaches_the_read() {
    let d = std::env::temp_dir().join(format!(
        "nn-dirboth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // The store side: a task output whose interior path is a DIRECTORY,
    // spelled as the round's own shape.
    let store_out = d.join("ninja-build-include-kernel-abi/include/kernel-abi");
    fs::create_dir_all(&store_out).unwrap();
    fs::create_dir_all(d.join("build")).unwrap();
    // The build side: the path the graph declares for that output.
    let build_side = d.join("build/include/kernel-abi");
    fs::create_dir_all(&build_side).unwrap();

    let mut built_paths = std::collections::HashMap::new();
    built_paths.insert(build_side.clone(), store_out.clone());

    let got = deps_infer::c_include_parser::retrieve_c_includes_checked(
        "gcc -c x.c -o x.o",
        built_paths.keys().cloned().collect(),
        Some(built_paths),
    );
    match got {
        Ok(_) => panic!("expected the directory output to reach the read"),
        Err(e) => {
            let err = format!("{e:#}");
            // Printed, not only asserted: this is the one arm that claims to
            // reproduce a round, and a reader should see the text rather
            // than trust a substring match on it.
            println!("reproduced: {err}");
            assert!(
                err.contains("Is a directory") || err.contains("os error 21"),
                "expected the round's EISDIR, got: {err}"
            );
        }
    }

    let _ = fs::remove_dir_all(&d);
}
