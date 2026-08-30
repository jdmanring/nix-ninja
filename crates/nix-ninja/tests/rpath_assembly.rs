//! The RPATH retention rule, pinned WHERE IT IS APPLIED.
//!
//! `crates/nix-ninja-task` already has nine tests for `retained_entries`, the
//! extracted rule. None of them covered the fact that the assembly CALLS it:
//! a mutation replacing the call with `current_rpath.to_vec()` left the whole
//! suite green while restoring the defect `f8bb3bd` fixed. The build succeeds
//! and the store output carries an empty RPATH element, which the dynamic
//! loader reads as the CURRENT DIRECTORY - library injection, in a path other
//! derivations link against.
//!
//! This file lives in `crates/nix-ninja` on purpose. `nix-ninja-task`'s `src`
//! fileset allowlist covers its own crate, so a test added there re-keys the
//! task binary and every per-TU output a consumer has already built. Reaching
//! the same `pub` function from here costs nothing.

use nix_ninja_task::patchelf::assemble_rpath;

#[test]
fn an_empty_entry_never_survives_assembly() {
    // The shape a linker leaves behind: a trailing colon becomes an empty
    // element, and to the loader an empty element means `.`.
    let current = vec![
        String::new(),
        "/build/source/build/lib".to_string(),
        "$ORIGIN/../lib".to_string(),
    ];
    let needed = vec!["/nix/store/aaaa-glibc/lib".to_string()];

    let got = assemble_rpath(&current, &needed).expect("a new dir was added, so Some");

    assert!(
        !got.iter().any(|e| e.is_empty()),
        "an empty RPATH element is a current-directory search: {got:?}"
    );
    assert!(
        !got.iter().any(|e| e.contains("$ORIGIN")),
        "$ORIGIN is relative to the OUTPUT, which moves into the store: {got:?}"
    );
    assert!(
        got.contains(&"/nix/store/aaaa-glibc/lib".to_string()),
        "the resolved library directory must be kept: {got:?}"
    );
}

#[test]
fn nothing_new_means_leave_the_binary_alone() {
    // Not merely "empty result": returning Some here would rewrite a binary
    // that needed no rewrite, and every rewrite is a chance to get it wrong.
    let current = vec!["/nix/store/aaaa-glibc/lib".to_string()];
    let needed = vec!["/nix/store/aaaa-glibc/lib".to_string()];
    assert!(assemble_rpath(&current, &needed).is_none());
}

#[test]
fn a_binary_whose_only_entries_are_dropped_still_gains_what_it_needs() {
    // The regression this pair exists for: if retention were skipped, the
    // output would keep the empty entry AND gain the store dir, and the test
    // above would be the only thing standing between that and a shipped
    // artifact.
    let current = vec![String::new(), "$ORIGIN".to_string()];
    let needed = vec!["/nix/store/bbbb-zlib/lib".to_string()];
    let got = assemble_rpath(&current, &needed).unwrap();
    assert_eq!(got, vec!["/nix/store/bbbb-zlib/lib".to_string()]);
}
