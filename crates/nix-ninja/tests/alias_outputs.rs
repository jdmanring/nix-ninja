//! An output that is a SYMLINK is a name, not a product.
//!
//! A link edge declares `libfoo.so.1.2.3`, `libfoo.so.1` and `libfoo.so` as
//! three outputs, and its command writes one library and two links to it.
//! Storing those by copying through the link gave three real files of
//! identical size: the soname relationship is lost, and CMake's install-time
//! RPATH rewrite skips a path it believes is a link, so the alias shipped
//! with its build-tree RPATH intact and nixpkgs' forbidden-reference audit
//! refused brotli.
//!
//! In `crates/nix-ninja` on purpose: `nix-ninja-task`'s fileset allowlist
//! covers its own crate, so a test added there re-keys every banked per-TU
//! output. These reach the same `pub` functions from outside it.

use nix_ninja_task::derived_file::{alias_link_target, placement_link_text};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nn-alias-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// THE SHAPE BROTLI HAS. `cmake -E cmake_symlink_library` writes the library
/// and links the two alias names at it in the same directory.
#[test]
fn a_sibling_alias_is_stored_as_its_link_text() {
    let d = dir("sibling");
    std::fs::write(d.join("libfoo.so.1.2.3"), b"ELF").unwrap();
    std::os::unix::fs::symlink("libfoo.so.1.2.3", d.join("libfoo.so.1")).unwrap();

    assert_eq!(
        alias_link_target(&d.join("libfoo.so.1")),
        Some(PathBuf::from("libfoo.so.1.2.3")),
        "the TEXT is what is stored: each output is its own store object, so \
         a resolved path would name a directory the sibling is not in"
    );
    // The library itself is a product and must still be stored as content.
    assert_eq!(alias_link_target(&d.join("libfoo.so.1.2.3")), None);
    std::fs::remove_dir_all(&d).ok();
}

/// THE CONFINEMENT, and it is the half that keeps this from reaching outside
/// the build tree. A target with a separator names something no placement
/// recreates, so it is stored as content exactly as before.
#[test]
fn a_target_that_could_climb_out_is_not_treated_as_an_alias() {
    let d = dir("escape");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("real"), b"ELF").unwrap();

    std::os::unix::fs::symlink("../real", d.join("sub").join("up")).unwrap();
    assert_eq!(alias_link_target(&d.join("sub").join("up")), None);

    std::os::unix::fs::symlink("sub/deep", d.join("down")).unwrap();
    assert_eq!(alias_link_target(&d.join("down")), None);

    std::os::unix::fs::symlink(d.join("real"), d.join("abs")).unwrap();
    assert_eq!(alias_link_target(&d.join("abs")), None);
    std::fs::remove_dir_all(&d).ok();
}

/// THE OTHER HALF, AND WITHOUT IT THE FIRST ONE SHIPS A DANGLING LINK. The
/// stored object is itself a symlink naming a sibling that exists in the
/// BUILD directory, not beside the store object. A placement pointing at the
/// store object gives a link to a link to nothing.
#[test]
fn placement_carries_the_text_rather_than_the_store_path() {
    let d = dir("place");
    let store = d.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let stored_alias = store.join("libfoo.so.1");
    std::os::unix::fs::symlink("libfoo.so.1.2.3", &stored_alias).unwrap();

    assert_eq!(
        placement_link_text(&stored_alias),
        PathBuf::from("libfoo.so.1.2.3"),
        "pointing at the store object would resolve the sibling inside \
         /nix/store, where it does not exist"
    );

    // An ordinary product is still linked AT its store path.
    let stored_file = store.join("libfoo.so.1.2.3");
    std::fs::write(&stored_file, b"ELF").unwrap();
    assert_eq!(placement_link_text(&stored_file), stored_file);
    std::fs::remove_dir_all(&d).ok();
}

/// THE ROUND TRIP, because the two halves are only correct together and each
/// passes its own test while the pair is broken. Store the alias, place both
/// outputs into a fresh build directory, and require the alias to resolve to
/// the library's content.
#[test]
fn the_alias_resolves_after_a_store_and_place_round_trip() {
    let d = dir("roundtrip");
    let build = d.join("build");
    let store = d.join("store");
    let out = d.join("out");
    for p in [&build, &store, &out] {
        std::fs::create_dir_all(p).unwrap();
    }

    // What the edge's command leaves behind.
    std::fs::write(build.join("libfoo.so.1.2.3"), b"ELF-LIBRARY").unwrap();
    std::os::unix::fs::symlink("libfoo.so.1.2.3", build.join("libfoo.so.1")).unwrap();

    // Store, the way copy_outputs_to_placeholders does.
    for name in ["libfoo.so.1.2.3", "libfoo.so.1"] {
        let src = build.join(name);
        let dst = store.join(name);
        match alias_link_target(&src) {
            Some(text) => std::os::unix::fs::symlink(text, &dst).unwrap(),
            None => {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }
    assert!(
        std::fs::symlink_metadata(store.join("libfoo.so.1"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the alias must reach the store as a link, or nothing downstream can \
         tell it from a product"
    );

    // Place, the way create_symlinks does.
    for name in ["libfoo.so.1.2.3", "libfoo.so.1"] {
        std::os::unix::fs::symlink(placement_link_text(&store.join(name)), out.join(name)).unwrap();
    }

    assert_eq!(
        std::fs::read(out.join("libfoo.so.1")).unwrap(),
        b"ELF-LIBRARY",
        "the placed alias must resolve to the library's content"
    );
    assert!(
        std::fs::symlink_metadata(out.join("libfoo.so.1"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "and must still BE a link, which is what makes CMake's install \
         rewrite skip it instead of shipping a build-tree RPATH"
    );
    // Not a copy: three real files of identical size is the signature this
    // whole change exists to prevent.
    let placed = std::fs::read_link(out.join("libfoo.so.1")).unwrap();
    assert_eq!(placed, Path::new("libfoo.so.1.2.3"));

    std::fs::remove_dir_all(&d).ok();
}

/// THE REGRESSION THE ALIAS FIX SHIPPED, and it is a class rather than a case.
///
/// Every declared output of an edge becomes its OWN store object, so an alias
/// stored as its link text is DANGLING when read in the store: the sibling it
/// names lives in a different store path. That is the correct state - the link
/// resolves once both are placed in a build directory - but `create_symlinks`
/// guarded its placement with `exists()`, which FOLLOWS the link and therefore
/// answers no about exactly the object the fix stores.
///
/// The producing package never sees it. brotli's own alias is consumed by
/// nothing within brotli, so brotli builds and the round-trip test passes;
/// the failure needs a CONSUMER that takes the alias as an input. In a live
/// round this took out fmt, gtest, json-c, lz4, snappy and libbrotlicommon
/// itself - the fix written for brotli made brotli's alias unusable
/// downstream.
///
/// The sibling is placed beside it because the driver expands an edge's
/// co-outputs into any consumer's inputs (`task.rs`, `co_outputs`), so the
/// recreated text resolves where the task runs.
#[test]
fn a_consumer_may_take_a_dangling_alias_as_an_input() {
    let d = std::env::temp_dir().join(format!("nn-alias-consumer-{}", std::process::id()));
    let store = d.join("store-libfoo.so");
    std::fs::create_dir_all(&store).unwrap();

    // The alias output exactly as it lands in the store: one entry, a
    // relative link to a sibling that is NOT in this store path.
    std::os::unix::fs::symlink("libfoo.so.1.2.3", store.join("libfoo.so")).unwrap();
    let alias = store.join("libfoo.so");

    assert!(
        !alias.exists(),
        "the guard's own predicate: exists() follows the link and says no"
    );
    assert!(
        std::fs::symlink_metadata(&alias).is_ok(),
        "while the LINK is plainly there, which is what the guard means to ask"
    );

    // What placement then produces, given the co-output is present.
    let out = d.join("build");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("libfoo.so.1.2.3"), b"ELF-LIBRARY").unwrap();
    std::os::unix::fs::symlink(placement_link_text(&alias), out.join("libfoo.so")).unwrap();

    assert_eq!(
        std::fs::read(out.join("libfoo.so")).unwrap(),
        b"ELF-LIBRARY",
        "placed beside its co-output, the alias resolves"
    );

    std::fs::remove_dir_all(&d).ok();
}
