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
    // $ORIGIN STAYS, AND THE ASSERTION HERE USED TO BE THE OPPOSITE. Its
    // reason was that the entry is relative to an output which moves into the
    // store, and that much is true: after the move it names a directory
    // inside our own output and usually resolves to nothing. What does not
    // follow is that removing it is safe. A stale relative entry inside our
    // own output is INERT, while the entry is CMake's record - it locates its
    // old RPATH as an entry sequence and refuses the install outright when it
    // is gone, which is how abseil-cpp failed on a library we had rewritten.
    // Retaining it also costs nothing at runtime, because CMake's own install
    // rewrite is what removes it.
    assert!(
        got.contains(&"$ORIGIN/../lib".to_string()),
        "$ORIGIN is CMake's record and is inert in a store output: {got:?}"
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
    //
    // The two entries are no longer treated alike, and that is the point. An
    // empty element is a CURRENT-DIRECTORY search and never survives;
    // `$ORIGIN` names the binary's own directory and does. One rule covered
    // both for as long as no package recorded the second.
    let current = vec![String::new(), "$ORIGIN".to_string()];
    let needed = vec!["/nix/store/bbbb-zlib/lib".to_string()];
    let got = assemble_rpath(&current, &needed).unwrap();
    assert_eq!(
        got,
        vec![
            "$ORIGIN".to_string(),
            "/nix/store/bbbb-zlib/lib".to_string()
        ]
    );
    assert!(!got.iter().any(|e| e.is_empty()));
}
