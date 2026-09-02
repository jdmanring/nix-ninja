//! The RPATH retention rule, pinned WHERE IT IS APPLIED.
//!
//! This file lives in `crates/nix-ninja` on purpose. `nix-ninja-task`'s `src`
//! fileset allowlist covers its own crate, so a test added there re-keys the
//! task binary and every per-TU output a consumer has already built. Reaching
//! the same `pub` function from here costs nothing.

use nix_ninja_task::patchelf::assemble_rpath;

/// THE CASE TWO PACKAGES DIED ON, and the reason this file's central
/// assertion is the reverse of what it was.
///
/// CMake reserves space for its install-time RPATH rewrite by padding the
/// build-time RPATH with a run of EMPTY entries sized to the install value,
/// and `file(RPATH_CHANGE)` records the padding as part of `OLD_RPATH`. The
/// rewrite is in place, so the reservation IS the allocation. Dropping the
/// padding removed it, and openexr and libjpeg-turbo both failed to install
/// a library this code had rewritten.
///
/// It read as two unrelated symptoms - one install cannot find its record,
/// another says the replacement is too long - and they are one arithmetic
/// seen from either side. openexr reserved 78 bytes for a 77-byte value; the
/// filter left 31.
#[test]
fn cmake_padding_survives_assembly() {
    let current = vec![
        "/build/source/build/src/lib/Iex".to_string(),
        String::new(),
        String::new(),
        String::new(),
    ];
    let needed = vec!["/nix/store/aaaa-glibc/lib".to_string()];

    let got = assemble_rpath(&current, &needed).expect("a new dir was added, so Some");

    assert_eq!(
        got.iter().filter(|e| e.is_empty()).count(),
        3,
        "the padding is CMake's byte reservation for its own rewrite, and \
         removing it is what refuses the install: {got:?}"
    );
    // The recorded sequence must still be findable, which means the padding
    // stays CONTIGUOUS with the entry it pads and anything we add goes after.
    assert!(
        got.join(":")
            .starts_with("/build/source/build/src/lib/Iex:::"),
        "CMake matches OLD_RPATH as an entry sequence: {got:?}"
    );
    assert!(
        got.contains(&"/nix/store/aaaa-glibc/lib".to_string()),
        "the resolved library directory must be kept: {got:?}"
    );
}

/// `$ORIGIN` STAYS, AND THE ASSERTION HERE USED TO BE THE OPPOSITE. Its
/// reason was that the entry is relative to an output which moves into the
/// store, and that much is true: after the move it names a directory inside
/// our own output and usually resolves to nothing. What does not follow is
/// that removing it is safe. A stale relative entry inside our own output is
/// INERT, while the entry is CMake's record - it locates its old RPATH as an
/// entry sequence and refuses the install outright when it is gone, which is
/// how abseil-cpp failed on a library we had rewritten.
#[test]
fn origin_survives_assembly() {
    let current = vec!["$ORIGIN/../lib".to_string()];
    let needed = vec!["/nix/store/aaaa-glibc/lib".to_string()];
    let got = assemble_rpath(&current, &needed).unwrap();
    assert_eq!(
        got,
        vec![
            "$ORIGIN/../lib".to_string(),
            "/nix/store/aaaa-glibc/lib".to_string()
        ]
    );
}

/// WHAT THE RULE COSTS, asserted rather than left to be discovered. An empty
/// entry means the current directory, so a binary CMake never rewrites
/// carries a cwd library search into the store.
///
/// Accepted, because the alternative is a third predicate over this one
/// string and the previous two both shipped defects - and because the case is
/// loud: a binary keeping an empty entry also keeps the build-tree entry
/// beside it, which is what nixpkgs' forbidden-reference audit refuses by
/// name. A predicate that guessed wrong would fail silently instead.
#[test]
fn an_empty_entry_with_no_padding_role_survives_too() {
    let current = vec![String::new(), "/build/source/build/lib".to_string()];
    let needed = vec!["/nix/store/bbbb-zlib/lib".to_string()];
    let got = assemble_rpath(&current, &needed).unwrap();
    assert_eq!(
        got,
        vec![
            String::new(),
            "/build/source/build/lib".to_string(),
            "/nix/store/bbbb-zlib/lib".to_string()
        ],
        "nothing here can tell CMake's padding from a build's own empty entry"
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
