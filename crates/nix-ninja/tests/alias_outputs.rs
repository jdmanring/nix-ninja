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

use nix_ninja_task::derived_file::{alias_link_target, placement_link_text, store_output};
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

    // Store through the task binary's own step, not a re-statement of it:
    // the production call once reverted to a copy with this test green.
    for name in ["libfoo.so.1.2.3", "libfoo.so.1"] {
        store_output(&build.join(name), &store.join(name)).unwrap();
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

/// THE PLACEMENT ITSELF, CALLED, which every test above this line avoids.
///
/// Three defects have now been fixed inside `create_symlinks` and not one of
/// them was covered by a test that RUNS it: the guard that refused a dangling
/// alias, and the duplicate check that called an alias a conflict with itself.
/// Both passed every predicate test in this file while the function was
/// broken. A predicate test states what a rule answers; only a test that
/// calls the function states that anything asks it.
///
/// The shape is gtest's, from a live round: an edge declares the versioned
/// library and its unversioned alias, the driver's co-output expansion hands a
/// consumer BOTH, and the alias is then placed by two routes - once as its
/// recreated link text and once as its own store object.
#[test]
fn placing_one_alias_by_two_routes_is_a_duplicate_not_a_conflict() {
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};

    let d = dir("wiring");
    let store_root = d.join("store");
    let build = d.join("build");
    std::fs::create_dir_all(&build).unwrap();

    // Two store objects, as an edge's two declared outputs really land.
    let h_lib = "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let h_alias = "1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let lib = store_root.join(format!("{h_lib}-ninja-build-lib-libgtest_main.so.1.17.0"));
    let alias = store_root.join(format!("{h_alias}-ninja-build-lib-libgtest_main.so"));
    std::fs::create_dir_all(lib.join("lib")).unwrap();
    std::fs::create_dir_all(alias.join("lib")).unwrap();
    std::fs::write(lib.join("lib/libgtest_main.so.1.17.0"), b"ELF-LIBRARY").unwrap();
    // The alias output: a relative link to a sibling in ANOTHER store object.
    std::os::unix::fs::symlink(
        "libgtest_main.so.1.17.0",
        alias.join("lib/libgtest_main.so"),
    )
    .unwrap();

    let store_dir = StoreDir::new(&store_root).unwrap();
    let enc = |h: &str, name: &str, rel: &str| {
        let sp = format!("{}/{h}-{name}", store_root.display());
        DerivedFile::from_encoded(&store_dir, &format!("{sp}:{rel}:{rel}")).unwrap()
    };
    let inputs = vec![
        enc(
            h_lib,
            "ninja-build-lib-libgtest_main.so.1.17.0",
            "lib/libgtest_main.so.1.17.0",
        ),
        enc(
            h_alias,
            "ninja-build-lib-libgtest_main.so",
            "lib/libgtest_main.so",
        ),
        // The SAME alias again, which is what the co-output expansion
        // produces and what aborted the round.
        enc(
            h_alias,
            "ninja-build-lib-libgtest_main.so",
            "lib/libgtest_main.so",
        ),
    ];

    create_symlinks(&build, &store_dir, inputs, false)
        .expect("one alias reaching the placer twice is a duplicate, not two claimants");

    assert_eq!(
        std::fs::read(build.join("lib/libgtest_main.so")).unwrap(),
        b"ELF-LIBRARY",
        "and the placed alias resolves through to the library"
    );

    std::fs::remove_dir_all(&d).ok();
}

/// `overwrite = true` IS A SECOND CALLER AND NOTHING EXERCISED IT.
///
/// The driver's local placement passes `overwrite=true`; the task binary
/// passes false into a fresh sandbox. Only the second was covered, and the
/// defect lived in the first: the branch asked `dest_path.exists()`, which
/// FOLLOWS the link, so a stale placement that dangles was skipped instead of
/// replaced and then reported as a conflict with the input replacing it.
///
/// The shape is an ordinary version bump: the alias text moves from
/// `libfoo.so.1.2.3` to `libfoo.so.1.2.4` and the old sibling is gone.
#[test]
fn a_stale_dangling_placement_is_replaced_rather_than_refused() {
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};

    let d = dir("overwrite");
    let store_root = d.join("store");
    let build = d.join("build");
    std::fs::create_dir_all(&build).unwrap();

    let h = "1ccccccccccccccccccccccccccccccc";
    let alias = store_root.join(format!("{h}-ninja-build-libfoo.so"));
    std::fs::create_dir_all(&alias).unwrap();
    std::os::unix::fs::symlink("libfoo.so.1.2.4", alias.join("libfoo.so")).unwrap();

    // What the previous run left: the OLD text, whose sibling is gone.
    std::os::unix::fs::symlink("libfoo.so.1.2.3", build.join("libfoo.so")).unwrap();
    assert!(
        !build.join("libfoo.so").exists(),
        "the stale placement dangles, which is what made the branch skip it"
    );
    std::fs::write(build.join("libfoo.so.1.2.4"), b"NEW-LIBRARY").unwrap();

    let store_dir = StoreDir::new(&store_root).unwrap();
    let df = DerivedFile::from_encoded(
        &store_dir,
        &format!(
            "{}/{h}-ninja-build-libfoo.so:libfoo.so:libfoo.so",
            store_root.display()
        ),
    )
    .unwrap();

    create_symlinks(&build, &store_dir, vec![df], true)
        .expect("a stale dangling placement must be replaced, not called a conflict");

    assert_eq!(
        std::fs::read(build.join("libfoo.so")).unwrap(),
        b"NEW-LIBRARY",
        "and the replacement must point at the new sibling"
    );
    std::fs::remove_dir_all(&d).ok();
}

/// THE NEGATIVE CASE THE WIDENING NEEDS, absent until an audit asked for it.
///
/// Reading one alias arriving twice as a duplicate is only safe if two
/// GENUINELY different objects still abort. Without this, a mutation widening
/// the comparison to "any symlink source is a duplicate" passes every other
/// test in this file, and the result is a silently wrong build rather than a
/// failed one.
#[test]
fn two_different_aliases_on_one_build_path_still_abort() {
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};

    let d = dir("conflict");
    let store_root = d.join("store");
    let build = d.join("build");
    std::fs::create_dir_all(&build).unwrap();

    let (h1, h2) = (
        "1ddddddddddddddddddddddddddddddd",
        "1fffffffffffffffffffffffffffffff",
    );
    for (h, target) in [(h1, "libfoo.so.1"), (h2, "libfoo.so.2")] {
        let p = store_root.join(format!("{h}-ninja-build-libfoo.so"));
        std::fs::create_dir_all(&p).unwrap();
        std::os::unix::fs::symlink(target, p.join("libfoo.so")).unwrap();
    }

    let store_dir = StoreDir::new(&store_root).unwrap();
    let enc = |h: &str| {
        DerivedFile::from_encoded(
            &store_dir,
            &format!(
                "{}/{h}-ninja-build-libfoo.so:libfoo.so:libfoo.so",
                store_root.display()
            ),
        )
        .unwrap()
    };

    let err = create_symlinks(&build, &store_dir, vec![enc(h1), enc(h2)], false)
        .expect_err("two different aliases claiming one build path is a real conflict");
    assert!(
        err.to_string().contains("claim one build path"),
        "and it must abort with the conflict message, not something else: {err}"
    );
    std::fs::remove_dir_all(&d).ok();
}

/// A TREE CO-OUTPUT REACHING THE OUTER BUILD DIRECTORY IS ADDITIVE.
///
/// Target resolution expands an edge's co-outputs, and one of the two
/// populations recorded in that map is the synthetic id of a DIRECTORY
/// output (a Qt `_autogen` tree, the syncqt include tree). A tree already
/// reached the outer build directory under `NIX_NINJA_MATERIALIZE_ALL`,
/// which hands every built file to the placement - `create_symlinks`'s
/// EISDIR case was measured there. What is new is that a PLAIN TARGET
/// reaches one, so the path no longer sits behind an environment variable
/// and lands on a directory holding cmake's configure-time content.
///
/// What must hold there is that the placement adds and never replaces: a
/// file the build tree already has is the authority, and the tree carries a
/// copy of every forwarding header. `link_tree` skips an existing entry and
/// has no abort path, so a directory input cannot produce the conflict a
/// file input can - asserted here rather than read, because the overwrite
/// guard three lines above it excludes directories for a different reason.
#[test]
fn a_tree_placed_over_a_populated_build_dir_adds_without_replacing() {
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};

    let d = dir("tree-outer");
    let store_root = d.join("store");
    let build = d.join("build");
    let hash = "1ccccccccccccccccccccccccccccccc";

    let tree = store_root.join(format!("{hash}-ninja-build-gen"));
    std::fs::create_dir_all(tree.join("gen")).unwrap();
    std::fs::write(tree.join("gen/kept.h"), b"from the store").unwrap();
    std::fs::write(tree.join("gen/new.h"), b"from the store").unwrap();

    // The build tree's own copy, which must survive.
    std::fs::create_dir_all(build.join("gen")).unwrap();
    std::fs::write(build.join("gen/kept.h"), b"from the build tree").unwrap();

    let store_dir = StoreDir::new(&store_root).unwrap();
    let df = DerivedFile::from_encoded(
        &store_dir,
        &format!("{}/{hash}-ninja-build-gen:gen:gen", store_root.display()),
    )
    .unwrap();

    create_symlinks(&build, &store_dir, vec![df], true)
        .expect("a directory co-output places file by file rather than aborting");

    assert_eq!(
        std::fs::read(build.join("gen/kept.h")).unwrap(),
        b"from the build tree",
        "an entry the build tree already holds is the authority"
    );
    assert!(
        build.join("gen/new.h").exists(),
        "and the entries it does not hold are placed"
    );
    std::fs::remove_dir_all(&d).ok();
}
