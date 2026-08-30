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

// UPSTREAM #41: "Test n2 with mmap?"
//
// The question has a catch that decides the answer, and it is not about
// speed. n2's scanner requires its buffer to end in a trailing NUL - that is
// the whole reason `read_file_with_nul` exists rather than `fs::read`. An
// mmap cannot append a byte to a file it maps.
//
// It gets one for free ONLY when the file's length is not a multiple of the
// page size: the kernel zero-fills the remainder of the final page, so the
// byte just past EOF is a readable NUL. When the length IS an exact multiple
// there is no such byte, and reading it is a SIGBUS rather than a wrong
// answer - which is the dangerous kind of rare, because a corpus of build
// files will hit it roughly once every 4096 files and not on the developer's
// machine.
//
// So an mmap scanner needs a fallback for that case whatever the timings say.
// These two benches measure whether the speed is worth carrying it.
fn page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// mmap a file and hand back a slice INCLUDING the zero byte past EOF.
/// Returns None when the length is a page multiple, which is the case that
/// has no free NUL and must fall back to a read.
unsafe fn mmap_with_nul(path: &Path) -> Result<(*mut libc::c_void, usize, usize), &'static str> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let fd = libc::open(c.as_ptr(), libc::O_RDONLY);
    if fd < 0 {
        return Err("open failed");
    }
    // metadata() on the PATH would open a second time and leak nothing, but
    // it also races the open; and a failure here used to be reported to the
    // caller as "page multiple", which sent the reader to regenerate a
    // fixture that was fine. Three causes, three messages.
    let len = match fs::metadata(path) {
        Ok(m) => m.len() as usize,
        Err(_) => {
            libc::close(fd);
            return Err("stat failed");
        }
    };
    if len.is_multiple_of(page_size()) {
        libc::close(fd);
        return Err("length is a page multiple: no free NUL past EOF");
    }
    let map_len = len + 1;
    let p = libc::mmap(
        std::ptr::null_mut(),
        map_len,
        libc::PROT_READ,
        libc::MAP_PRIVATE,
        fd,
        0,
    );
    libc::close(fd);
    if p == libc::MAP_FAILED {
        return Err("mmap failed");
    }
    Ok((p, len, map_len))
}

#[divan::bench]
fn read_10k_ninja_to_buffer_with_nul() {
    let p = fixture().join("build-10000.ninja");
    // The shape read_file_with_nul uses after the uninit_vec repair.
    let size = fs::metadata(&p).unwrap().len() as usize;
    let mut bytes = Vec::with_capacity(size + 1);
    let mut f = fs::File::open(&p).unwrap();
    std::io::Read::read_to_end(&mut f, &mut bytes).unwrap();
    bytes.push(0);
    // Same reduction as the mmap bench, so the two differ only in how the
    // bytes arrive.
    let mut acc = 0u8;
    for b in bytes[..bytes.len() - 1].iter() {
        acc ^= *b;
    }
    divan::black_box(acc);
}

#[divan::bench]
fn mmap_10k_ninja_with_nul_past_eof() {
    let p = fixture().join("build-10000.ninja");
    unsafe {
        match mmap_with_nul(&p) {
            Ok((ptr, len, map_len)) => {
                let s = std::slice::from_raw_parts(ptr as *const u8, len + 1);
                assert_eq!(s[len], 0, "byte past EOF must be the free NUL");
                // TOUCH EVERY BYTE. The first version of this bench read
                // s[0] and s[len] and nothing else, so it faulted in two
                // pages and timed open+mmap+munmap while the read bench it
                // was compared against moved 400 KB. The comparison was
                // meaningless and its number reached a document.
                let mut acc = 0u8;
                for b in s[..len].iter() {
                    acc ^= *b;
                }
                divan::black_box(acc);
                libc::munmap(ptr, map_len);
            }
            Err(why) => panic!("mmap_with_nul: {why}"),
        }
    }
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
