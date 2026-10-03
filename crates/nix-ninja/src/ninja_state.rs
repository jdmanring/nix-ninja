//! NINJA'S OWN STATE FILES, WRITTEN FOR A TREE THIS DRIVER BUILT.
//!
//! A caller that asks real ninja whether a tree is up to date (`ninja -n`,
//! meson-python's editable install) gets its answer from `.ninja_log` and
//! `.ninja_deps`, not from the outputs existing. Measured on ninja 1.13.2:
//! with the outputs in place, deleting the log makes every edge dirty and
//! deleting the deps log makes every `deps = gcc` edge dirty. This driver wrote
//! neither, so real ninja reported work for every tree it built.
//!
//! WHY THIS CANNOT MAKE A TREE WITH WORK READ CLEAN. Each record asserts only
//! that an output exists, was produced by a command, and has an mtime; ninja
//! re-checks all three itself (`RecomputeOutputDirty`, `LoadDepsFromLog`):
//! an input newer than the output, a command hashing differently, a deps
//! record older than its output, a dependency that is missing - each still
//! reads as dirty. So every way this file can be wrong fails toward "work to
//! do", and an edge is recorded only when every one of its outputs was placed
//! by THIS run.
//!
//! Formats are ninja 1.13.2's, read from its source: `build_log.cc`
//! (`# ninja log v7`, start, end, mtime in ns, path, rapidhash of
//! `Edge::EvaluateCommand(true)` in hex) and `deps_log.cc` (`# ninjadeps`,
//! version 4, path and deps records).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One non-phony edge whose outputs were placed by this run.
#[derive(Debug, Clone)]
pub struct Edge {
    pub outputs: Vec<PathBuf>,
    /// The evaluated command, with `;rspfile=<content>` appended when the
    /// edge has a response file, which is the string ninja hashes.
    pub command: String,
    /// The edge's depfile when it is `deps = gcc`: ninja keeps its contents
    /// in the deps log and treats the edge as dirty without a record.
    pub gcc_depfile: Option<PathBuf>,
}

static COLLECTED: Mutex<Vec<Edge>> = Mutex::new(Vec::new());

pub fn collect(edges: Vec<Edge>) {
    *COLLECTED.lock().unwrap() = edges;
}

pub fn take_collected() -> Vec<Edge> {
    std::mem::take(&mut *COLLECTED.lock().unwrap())
}

/// ninja's command hash: rapidhash as vendored by ninja 1.13.2
/// (`third_party/rapidhash`, RAPIDHASH_FAST, little endian, default seed and
/// secrets; unrolling does not change the result).
pub fn rapidhash(data: &[u8]) -> u64 {
    const SECRET: [u64; 3] = [0x2d358dccaa6c78a5, 0x8bb84b93962eacc9, 0x4b33a62ed433d4a3];
    fn mum(a: u64, b: u64) -> (u64, u64) {
        let r = (a as u128) * (b as u128);
        (r as u64, (r >> 64) as u64)
    }
    fn mix(a: u64, b: u64) -> u64 {
        let (lo, hi) = mum(a, b);
        lo ^ hi
    }
    let r64 = |p: &[u8], i: usize| u64::from_le_bytes(p[i..i + 8].try_into().unwrap());
    let r32 = |p: &[u8], i: usize| u32::from_le_bytes(p[i..i + 4].try_into().unwrap()) as u64;
    let len = data.len();
    let mut seed = 0xbdd89aa982704029u64;
    seed ^= mix(seed ^ SECRET[0], SECRET[1]) ^ len as u64;
    let (a, b);
    if len <= 16 {
        if len >= 4 {
            let last = len - 4;
            a = (r32(data, 0) << 32) | r32(data, last);
            let delta = (len & 24) >> (len >> 3);
            b = (r32(data, delta) << 32) | r32(data, last - delta);
        } else if len > 0 {
            a = ((data[0] as u64) << 56) | ((data[len >> 1] as u64) << 32) | data[len - 1] as u64;
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut p = 0usize;
        let mut i = len;
        if i > 48 {
            let (mut see1, mut see2) = (seed, seed);
            while i >= 48 {
                seed = mix(r64(data, p) ^ SECRET[0], r64(data, p + 8) ^ seed);
                see1 = mix(r64(data, p + 16) ^ SECRET[1], r64(data, p + 24) ^ see1);
                see2 = mix(r64(data, p + 32) ^ SECRET[2], r64(data, p + 40) ^ see2);
                p += 48;
                i -= 48;
            }
            seed ^= see1 ^ see2;
        }
        if i > 16 {
            seed = mix(
                r64(data, p) ^ SECRET[2],
                r64(data, p + 8) ^ seed ^ SECRET[1],
            );
            if i > 32 {
                seed = mix(r64(data, p + 16) ^ SECRET[2], r64(data, p + 24) ^ seed);
            }
        }
        a = r64(data, p + i - 16);
        b = r64(data, p + i - 8);
    }
    let (a, b) = mum(a ^ SECRET[1], b ^ seed);
    mix(a ^ SECRET[0] ^ len as u64, b ^ SECRET[1])
}

/// Prerequisites of every rule in a depfile, through n2's own parser (the one
/// `depfile_read_back` uses), canonicalised as n2 names graph files. None when
/// the file is absent or does not parse: no record, which ninja reads as dirty.
fn depfile_inputs(path: &Path) -> Option<Vec<String>> {
    let buf = n2::scanner::read_file_with_nul(path).ok()?;
    let mut sc = n2::scanner::Scanner::new(&buf);
    let parsed = n2::depfile::parse(&mut sc).ok()?;
    Some(
        parsed
            .iter()
            .flat_map(|(_, values)| values.iter())
            .map(|v| n2::canon::to_owned_canon_path(*v))
            .collect(),
    )
}

fn mtime_ns(path: &Path) -> Option<i64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(d.as_nanos()).ok()
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("nn-tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Stamp every output with one mtime, then write both files from scratch.
/// Returns how many edges were recorded.
///
/// ONE MTIME, because placement order is not dependency order: a library
/// placed before its objects would read older than its own inputs. Equal is
/// not older in ninja's comparison, and a source edited afterwards is newer.
/// ponytail: rewritten whole each run, so an edge built by an earlier run and
/// not placed by this one loses its record and reads dirty; merge with the
/// existing files if partial-target runs ever need a clean answer.
pub fn write(build_dir: &Path, edges: &[Edge]) -> std::io::Result<usize> {
    let now = std::time::SystemTime::now();
    let mut log = String::from("# ninja log v7\n");
    let mut deps: Vec<u8> = b"# ninjadeps\n".to_vec();
    deps.extend_from_slice(&4i32.to_le_bytes());
    let mut ids: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
    let mut id_of = |path: String, deps: &mut Vec<u8>| -> i32 {
        if let Some(id) = ids.get(&path) {
            return *id;
        }
        let id = ids.len() as i32;
        let pad = (4 - path.len() % 4) % 4;
        deps.extend_from_slice(&((path.len() + pad + 4) as u32).to_le_bytes());
        deps.extend_from_slice(path.as_bytes());
        deps.extend(std::iter::repeat_n(0u8, pad));
        deps.extend_from_slice(&(!(id as u32)).to_le_bytes());
        ids.insert(path, id);
        id
    };
    let mut recorded = 0usize;
    for edge in edges {
        let abs: Vec<PathBuf> = edge.outputs.iter().map(|o| build_dir.join(o)).collect();
        // Every output or none: an edge with one output missing is dirty to
        // ninja whatever the log says, so recording it would only be noise.
        if abs.iter().any(|p| !p.exists()) {
            continue;
        }
        let mut stamped = Vec::with_capacity(abs.len());
        for p in &abs {
            let ok = std::fs::File::open(p)
                .and_then(|f| f.set_modified(now))
                .is_ok();
            match mtime_ns(p) {
                Some(m) if ok => stamped.push(m),
                _ => break,
            }
        }
        if stamped.len() != abs.len() {
            continue;
        }
        let hash = rapidhash(edge.command.as_bytes());
        for (out, m) in edge.outputs.iter().zip(&stamped) {
            log.push_str(&format!("0\t0\t{m}\t{}\t{hash:x}\n", out.display()));
        }
        if let Some(depfile) = &edge.gcc_depfile {
            // deps = gcc is single-output in ninja; a missing or unreadable
            // depfile leaves no record, which ninja reads as dirty.
            if let (Some(inputs), [out]) =
                (depfile_inputs(&build_dir.join(depfile)), &edge.outputs[..])
            {
                let out_id = id_of(
                    n2::canon::to_owned_canon_path(out.to_string_lossy()),
                    &mut deps,
                );
                let ins: Vec<i32> = inputs.into_iter().map(|i| id_of(i, &mut deps)).collect();
                deps.extend_from_slice(&(((3 + ins.len()) * 4) as u32 | 0x8000_0000).to_le_bytes());
                deps.extend_from_slice(&out_id.to_le_bytes());
                let m = stamped[0] as u64;
                deps.extend_from_slice(&((m & 0xffff_ffff) as u32).to_le_bytes());
                deps.extend_from_slice(&((m >> 32) as u32).to_le_bytes());
                for i in ins {
                    deps.extend_from_slice(&i.to_le_bytes());
                }
            }
        }
        recorded += 1;
    }
    write_atomically(&build_dir.join(".ninja_log"), log.as_bytes())?;
    write_atomically(&build_dir.join(".ninja_deps"), &deps)?;
    Ok(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors are real ninja 1.13.2's own `.ninja_log` hashes, one per
    /// branch of the length switch, read from logs it wrote.
    #[test]
    fn rapidhash_matches_ninja() {
        assert_eq!(rapidhash(b"touch t.txt"), 0x4c4a37cb571b2571);
        assert_eq!(
            rapidhash(b"gcc -MD -MF a.o.d -c a.c -o a.o"),
            0xa4e9afbeb13d95ec
        );
        assert_eq!(rapidhash(b":"), 0xa2da22321247804b);
        assert_eq!(rapidhash(b": x"), 0xef179f5022af3256);
        let c60 = format!(": {}", "a".repeat(58));
        assert_eq!(rapidhash(c60.as_bytes()), 0xcdb09cd9c5520083);
        let c150 = format!(": {}", "b".repeat(148));
        assert_eq!(rapidhash(c150.as_bytes()), 0x4f1d1276688e541b);
        // The response-file form `Edge::EvaluateCommand(true)` hashes.
        assert_eq!(
            rapidhash(b": @orsp.rsp;rspfile=-DX=1 orsp"),
            0x611f18524300639
        );
    }
}
