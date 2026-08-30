//! Benchmarks for derivation GENERATION - upstream nix-ninja #4.
//!
//! #4 asks for benchmarks of "nix-ninja generating derivations to compile
//! NixOS/Nix". This covers the part of that which runs without a daemon: the
//! ninja graph load and the include scan. Those are the two phases the
//! driver's own `RESOLVE_MS` breakdown attributes its serial cost to, and
//! they are the phases a change to this crate can actually move.
//!
//! What it deliberately does NOT cover, so no reader mistakes the scope: the
//! daemon round trip per task. That is `add_drv_to_store`, it is the cost
//! `DYN_PLAIN_ADDDRV_MS` was added to measure, and it cannot be benchmarked
//! here because it needs a store and a daemon. Benchmarking it belongs with
//! #7's end-to-end harness, not this file. A number from here is a claim
//! about the driver's CPU, never about a build's wall clock.
//!
//! Run with `cargo bench -p nix-ninja`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn main() {
    divan::main();
}

/// One fixture tree for the whole run, built once.
///
/// Generated rather than checked in: a corpus large enough to be worth timing
/// is not worth storing in git, and the generator states the shape it is
/// claiming to represent, which a checked-in blob would not.
fn fixture() -> &'static Path {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nix-ninja-bench-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("include")).unwrap();

        for size in [1_000usize, 10_000] {
            let mut b = String::from("rule cc\n  command = cc -c $in -o $out\n\n");
            for i in 0..size {
                b.push_str(&format!("build obj/f{i}.o: cc src/f{i}.c\n"));
            }
            fs::write(dir.join(format!("build-{size}.ninja")), b).unwrap();
        }

        // A header graph with fan-in: every source pulls a shared header that
        // itself includes others, which is the shape that makes the scan cost
        // something. A flat list of independent headers would understate it.
        fs::write(
            dir.join("include/common.h"),
            (0..32)
                .map(|i| format!("#include \"leaf{i}.h\"\n"))
                .collect::<String>(),
        )
        .unwrap();
        for i in 0..32 {
            fs::write(
                dir.join(format!("include/leaf{i}.h")),
                format!("#define LEAF{i} {i}\n"),
            )
            .unwrap();
        }
        for i in 0..64 {
            fs::write(
                dir.join(format!("tu{i}.c")),
                format!("#include \"common.h\"\nint f{i}(void) {{ return LEAF0; }}\n"),
            )
            .unwrap();
        }
        dir
    })
}

#[divan::bench]
fn ninja_graph_load_1k() {
    let p = fixture().join("build-1000.ninja");
    divan::black_box(n2::load::read(p.to_str().unwrap()).unwrap());
}

#[divan::bench]
fn ninja_graph_load_10k() {
    let p = fixture().join("build-10000.ninja");
    divan::black_box(n2::load::read(p.to_str().unwrap()).unwrap());
}

#[divan::bench]
fn include_scan_64_tus_shared_header() {
    let dir = fixture();
    let files: Vec<PathBuf> = (0..64).map(|i| dir.join(format!("tu{i}.c"))).collect();
    let cmdline = format!("cc -I{} -c x.c -o x.o", dir.join("include").display());
    divan::black_box(
        deps_infer::c_include_parser::retrieve_c_includes(&cmdline, files, None).unwrap(),
    );
}
