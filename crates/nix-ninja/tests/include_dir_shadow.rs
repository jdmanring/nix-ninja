//! A DIRECTORY ON THE INCLUDE PATH MUST NOT RESOLVE AS A HEADER.
//!
//! webrtc-audio-processing (1.3 and 2.1) failed every compile edge with
//! `Failed to read file /build/source/webrtc/rtc_base/memory: Is a
//! directory (os error 21)`: `#include <memory>` matched the
//! `rtc_base/memory/` SUBDIRECTORY, `canonicalize_cached`'s `exists()`
//! accepted it, the include BFS queued it, and `scan_directives`' read
//! died EISDIR.
//!
//! It lives in `crates/nix-ninja` rather than beside its subject in
//! `crates/deps-infer` on purpose: deps-infer is inside
//! `nix-ninja-task`'s `src` allowlist, so a test file there re-keys the
//! task binary and every banked per-TU output. See CLAUDE.md.

use deps_infer::c_include_parser::extract_includes;
use std::path::PathBuf;

#[test]
fn a_directory_shadowing_a_header_name_is_skipped() {
    let d = std::env::temp_dir().join(format!("nn-dirshadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let first = d.join("first");
    let second = d.join("second");
    // The trap: `first/memory` is a DIRECTORY, as rtc_base/memory is.
    std::fs::create_dir_all(first.join("memory")).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    // The real header, on a LATER include dir - which is what the
    // compiler finds and what the scan must declare.
    std::fs::write(second.join("memory"), "#include \"marker.h\"\n").unwrap();
    let src = d.join("platform_thread.cc");
    std::fs::write(&src, "#include <memory>\n").unwrap();

    let dirs = vec![first.clone(), second.clone()];
    let got = extract_includes(&src, &src, &dirs, None).expect("scan must not fail");

    assert!(
        !got.iter().any(|p| p.ends_with("first/memory")),
        "the directory was resolved as an include: {got:?}"
    );
    assert!(
        got.iter().any(|p| p.ends_with("second/memory")),
        "search did not fall through to the real header: {got:?}"
    );

    // And the whole chain: the queued path is read, so a directory here
    // is the EISDIR that killed the package.
    for p in &got {
        assert!(
            std::fs::read(p).is_ok(),
            "a resolved include cannot be read: {}",
            p.display()
        );
    }
    let _ = std::fs::remove_dir_all(&d);
    let _: PathBuf = second;
}
