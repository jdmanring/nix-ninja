//! A GENERATED HEADER SPELLED WITH `..` NEVER MATCHES ITS DECLARATION.
//!
//! `virtual_paths` is keyed by the path the build graph declares, and
//! `canonicalize_cached` probes it with `include_dir.join(spelling)` exactly
//! as written. An include spelled `../config.h` therefore probes
//! `build/coregrind/../config.h` while the graph declares `build/config.h`,
//! the lookup misses, and the file is scanned off a disk that does not have
//! it yet.
//!
//! `lexical_normalize` already exists a few lines above for this, and is
//! applied to the SPELLING recorded for the compiler and not to the KEY. The
//! two are different paths through the same function.
//!
//! Reached from `crates/nix-ninja` on purpose: the fix lives in
//! `crates/deps-infer`, inside `nix-ninja-task`'s fileset, and a test beside
//! it would re-key the task binary a second time for coverage that runs in no
//! build.
//!
//! valgrind spells its config header this way and is opted out of ArtNix for
//! it; dav1d, p11-kit and svt-av1 are the same class one spelling out.

use deps_infer::c_include_parser::canonicalize_cached;
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn a_dotdot_spelling_resolves_to_its_declared_generated_header() {
    let declared = PathBuf::from("/b/config.h");
    let mut vp = HashMap::new();
    vp.insert(declared.clone(), declared.clone());

    // What a compile edge in `/b/coregrind` does with `#include "../config.h"`.
    let probed = PathBuf::from("/b/coregrind/../config.h");
    let got = canonicalize_cached(probed, Some(&vp)).unwrap();

    assert_eq!(
        got,
        Some(declared),
        "a `..` spelling missed the virtual map, so the header is scanned off \
         disk before the build has written it"
    );
}

#[test]
fn a_dot_spelling_resolves_to_its_declared_generated_header() {
    let declared = PathBuf::from("gen/version.h");
    let mut vp = HashMap::new();
    vp.insert(declared.clone(), declared.clone());

    let got = canonicalize_cached(PathBuf::from("./gen/version.h"), Some(&vp)).unwrap();
    assert_eq!(got, Some(declared), "a leading `./` missed the virtual map");
}

/// A preprocessed Fortran source depends on the file its line markers name,
/// and no include directive mentions it. Reached from `crates/nix-ninja` for
/// the same reason as the tests above.
#[test]
fn a_preprocessed_fortran_source_declares_the_original_it_was_made_from() {
    use deps_infer::c_include_parser::retrieve_c_includes;

    let dir = std::env::temp_dir().join(format!("nn-fpp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let original = dir.join("dtpmv.f");
    std::fs::write(&original, "      end\n").unwrap();
    let pp = dir.join("dtpmv.f-pp.f");
    std::fs::write(&pp, format!("# 1 \"{}\"\n      end\n", original.display())).unwrap();

    let got = retrieve_c_includes(
        &format!("gfortran -fpreprocessed -c {}", pp.display()),
        vec![pp.clone()],
        None,
    )
    .unwrap();

    assert!(
        !got.iter().any(|p| p.to_string_lossy().starts_with('<')),
        "a preprocessor pseudo-file was declared as an input: {got:?}"
    );
    assert!(
        got.contains(&original),
        "the original named in the line markers was not declared, so the \
         compile runs in a sandbox without it: {got:?}"
    );

    // NEGATIVE CONTROL, and it is the half that matters: the same markers in
    // a file that is not CMake's `-pp.` spelling must declare nothing, or
    // every preprocessed C source drags its whole system include set in.
    let plain = dir.join("plain.f");
    std::fs::write(
        &plain,
        // `<built-in>` beside the real one: the preprocessor names itself in
        // these markers, and queuing that as a file fails the whole edge
        // rather than the lookup. Measured on liblapack's configure.
        format!(
            "# 1 \"<built-in>\"\n# 1 \"{}\"\n      end\n",
            original.display()
        ),
    )
    .unwrap();
    let got = retrieve_c_includes(
        &format!("gfortran -c {}", plain.display()),
        vec![plain],
        None,
    )
    .unwrap();
    assert!(
        !got.contains(&original),
        "line markers were followed outside the -pp. spelling: {got:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
