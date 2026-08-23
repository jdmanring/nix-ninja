//! Cross-round persistence for the expensive resolve memos.
//!
//! The python-closure and dir-arg memos in task.rs are process-local, so
//! every driver restart re-walks and re-uploads the same directories:
//! measured 76 s of resolve time to re-reach task 7,500 in round 67, all
//! of it store round-trips for content the daemon already holds. This
//! module appends each memo entry to a log beside build.ninja as it is
//! computed and replays it on the next start, turning a restart's py/dir
//! buckets into stat calls.
//!
//! Correctness posture: an entry is trusted only after validation at
//! first hit - every file it lists must still exist with the recorded
//! aggregate (count, total size, max mtime), and the key directory's own
//! mtime must match, so a changed or added file drops the entry and the
//! resolver recomputes fresh. The failure direction is re-upload, never
//! a stale store path. Store paths themselves stay valid because nothing
//! GCs mid-campaign; a GC'd input would fail the task derivation loudly,
//! same as for banked task derivations generally.
//!
//! The file carries a header naming the format version and DIR_UPLOAD_CAP;
//! a mismatch discards the whole file, because entries computed under a
//! different cap encode different truncation decisions.

use anyhow::Result;
use harmonia_store_path::{StoreDir, StorePath};
use nix_ninja_task::derived_file::DerivedFile;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const FILE_NAME: &str = ".nix-ninja-resolve-cache.v1";
/// Bumped with task.rs's DIR_UPLOAD_CAP so cap changes invalidate, and
/// with any change to what an upload CONTAINS: v2 discards entries
/// recorded before env-shebang patching, whose store paths hold the
/// unpatched bytes and would otherwise replay forever.
const HEADER: &str = "nix-ninja-resolve-cache v2 cap=1024";
/// Separates encoded DerivedFiles within one line; never appears in a
/// store path or a build-relative path.
const SEP: char = '\x1f';

struct Fingerprint {
    dir_mtime_ns: u128,
    count: usize,
    sum_size: u64,
    max_mtime_ns: u128,
}

struct Entry {
    fp: Fingerprint,
    encoded: Vec<String>,
}

struct Cache {
    store_dir: StoreDir,
    build_dir: PathBuf,
    path: PathBuf,
    /// Loaded from disk, not yet validated. Keyed by (kind, dir).
    unvalidated: Mutex<HashMap<(String, PathBuf), Entry>>,
    /// Lines computed this run, awaiting the next flush.
    pending: Mutex<Vec<String>>,
}

static CACHE: OnceLock<Option<Cache>> = OnceLock::new();

fn mtime_ns(p: &Path) -> Option<u128> {
    fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}

/// Aggregate over the files an entry lists, resolved against the build
/// dir (build_path is build-relative for opaque uploads). None if any
/// file is unreadable, which reads as "invalid" at the caller.
fn files_fingerprint(build_dir: &Path, key_dir: &Path, paths: &[PathBuf]) -> Option<Fingerprint> {
    let mut sum_size = 0u64;
    let mut max_mtime_ns = 0u128;
    for rel in paths {
        let p = build_dir.join(rel);
        let md = fs::metadata(&p).ok()?;
        sum_size += md.len();
        let m = md
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        max_mtime_ns = max_mtime_ns.max(m);
    }
    Some(Fingerprint {
        dir_mtime_ns: mtime_ns(&build_dir.join(key_dir))?,
        count: paths.len(),
        sum_size,
        max_mtime_ns,
    })
}

/// Call once from Runner::new. NIX_NINJA_RESOLVE_CACHE=0 disables.
pub fn init(store_dir: StoreDir, build_dir: PathBuf) {
    CACHE.get_or_init(|| {
        if std::env::var("NIX_NINJA_RESOLVE_CACHE").as_deref() == Ok("0") {
            return None;
        }
        let path = build_dir.join(FILE_NAME);
        let mut unvalidated = HashMap::new();
        if let Ok(body) = fs::read_to_string(&path) {
            let mut lines = body.lines();
            if lines.next() == Some(HEADER) {
                for line in lines {
                    let mut f = line.split('\t');
                    let (Some(kind), Some(key), Some(dm), Some(n), Some(sz), Some(mm), Some(enc)) = (
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                    ) else {
                        continue;
                    };
                    let (Ok(dir_mtime_ns), Ok(count), Ok(sum_size), Ok(max_mtime_ns)) =
                        (dm.parse(), n.parse(), sz.parse(), mm.parse())
                    else {
                        continue;
                    };
                    // Last write wins, same as the in-memory memos.
                    unvalidated.insert(
                        (kind.to_string(), PathBuf::from(key)),
                        Entry {
                            fp: Fingerprint {
                                dir_mtime_ns,
                                count,
                                sum_size,
                                max_mtime_ns,
                            },
                            encoded: enc.split(SEP).map(str::to_string).collect(),
                        },
                    );
                }
            } else {
                // Remove, don't just ignore: flush() writes the header only
                // when the file is absent, so appending under a stale header
                // strands every entry this run banks - measured round 71,
                // which banked 11,000 tasks' memos that round 72 then threw
                // away unread.
                let _ = fs::remove_file(&path);
                println!(
                    "nix-ninja: resolve cache {} had a different version/cap header; removed",
                    path.display()
                );
            }
        }
        let loaded = unvalidated.len();
        if loaded > 0 {
            println!("nix-ninja: resolve cache loaded, {loaded} directory entries");
        }
        Some(Cache {
            store_dir,
            build_dir,
            path,
            unvalidated: Mutex::new(unvalidated),
            pending: Mutex::new(Vec::new()),
        })
    });
}

/// A validated hit, or None (miss, invalidated, or cache disabled).
/// Validation stats every listed file; a mismatch drops the entry so
/// the caller recomputes and re-records.
pub fn lookup(kind: &str, key: &Path) -> Option<Vec<DerivedFile>> {
    let cache = CACHE.get()?.as_ref()?;
    let entry = cache
        .unvalidated
        .lock()
        .unwrap()
        .remove(&(kind.to_string(), key.to_path_buf()))?;
    let files: Vec<DerivedFile> = entry
        .encoded
        .iter()
        .filter_map(|e| DerivedFile::from_encoded(&cache.store_dir, e).ok())
        .collect();
    if files.len() != entry.encoded.len() {
        return None;
    }
    let rels: Vec<PathBuf> = files.iter().map(|f| f.build_path.clone()).collect();
    let fp = files_fingerprint(&cache.build_dir, key, &rels)?;
    if fp.dir_mtime_ns == entry.fp.dir_mtime_ns
        && fp.count == entry.fp.count
        && fp.sum_size == entry.fp.sum_size
        && fp.max_mtime_ns == entry.fp.max_mtime_ns
    {
        Some(files)
    } else {
        None
    }
}

/// Record a freshly computed memo entry for the next run.
pub fn record(kind: &str, key: &Path, files: &[DerivedFile]) {
    let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) else {
        return;
    };
    let rels: Vec<PathBuf> = files.iter().map(|f| f.build_path.clone()).collect();
    let Some(fp) = files_fingerprint(&cache.build_dir, key, &rels) else {
        return;
    };
    let enc: Vec<String> = files
        .iter()
        .map(|f| f.to_encoded(&cache.store_dir))
        .collect();
    let line = format!(
        "{kind}\t{}\t{}\t{}\t{}\t{}\t{}",
        key.display(),
        fp.dir_mtime_ns,
        fp.count,
        fp.sum_size,
        fp.max_mtime_ns,
        enc.join(&SEP.to_string()),
    );
    cache.pending.lock().unwrap().push(line);
}

/// Append pending entries. Called from the per-500-task progress tick,
/// so a killed driver loses at most the last tick's entries.
pub fn flush() -> Result<()> {
    let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) else {
        return Ok(());
    };
    let lines: Vec<String> = std::mem::take(&mut *cache.pending.lock().unwrap());
    if lines.is_empty() {
        return Ok(());
    }
    let new = !cache.path.exists();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cache.path)?;
    if new {
        writeln!(f, "{HEADER}")?;
    }
    for line in lines {
        writeln!(f, "{line}")?;
    }
    Ok(())
}

const NAR_FILE: &str = ".nix-ninja-nar-stamps.v1";
const NAR_HEADER: &str = "nix-ninja-nar-stamps v1";

/// The previous run's NAR stamp snapshot, filtered to store paths that
/// still exist on disk (a GC between rounds drops the entry; the failure
/// direction is re-upload). Size+mtime are still validated per hit by
/// the client against the live file, same trust model as the memo
/// entries above.
pub fn load_nar_stamps() -> Vec<(PathBuf, u64, u128, StorePath)> {
    let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) else {
        return Vec::new();
    };
    let path = cache.build_dir.join(NAR_FILE);
    let Ok(body) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut lines = body.lines();
    if lines.next() != Some(NAR_HEADER) {
        let _ = fs::remove_file(&path);
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        let mut f = line.split('\t');
        let (Some(key), Some(sz), Some(mt), Some(sp)) = (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let (Ok(size), Ok(mtime)) = (sz.parse(), mt.parse()) else {
            continue;
        };
        let Ok(sp): std::result::Result<StorePath, _> = cache.store_dir.parse(sp) else {
            continue;
        };
        if !sp.to_absolute_path(&cache.store_dir).exists() {
            continue;
        }
        out.push((PathBuf::from(key), size, mtime, sp));
    }
    out
}

/// Rewrite the stamp file wholesale from the current snapshot, atomically
/// (tmp + rename), so a killed driver leaves the previous complete file
/// rather than a torn one. Keys with a tab or newline are unencodable and
/// skipped; no real path here carries either.
pub fn save_nar_stamps(entries: &[(PathBuf, u64, u128, StorePath)]) -> Result<()> {
    let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) else {
        return Ok(());
    };
    let path = cache.build_dir.join(NAR_FILE);
    let tmp = path.with_extension("v1.tmp");
    let mut body = String::from(NAR_HEADER);
    body.push('\n');
    for (key, size, mtime, sp) in entries {
        let k = key.display().to_string();
        if k.contains('\t') || k.contains('\n') {
            continue;
        }
        body.push_str(&format!(
            "{k}\t{size}\t{mtime}\t{}\n",
            cache.store_dir.display(sp)
        ));
    }
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // One process-wide CACHE means one test may init it; this test owns
    // it. Covers: parse, validated hit, and the negative control - an
    // entry whose recorded fingerprint no longer matches must return
    // None (the failure direction is recompute, never a stale path).
    #[test]
    fn validated_hit_and_stale_drop() {
        let bd = std::env::temp_dir().join(format!("nn-rc-{}", std::process::id()));
        fs::create_dir_all(bd.join("srcs")).unwrap();
        fs::write(bd.join("srcs/a.py"), "x = 1\n").unwrap();
        let md = fs::metadata(bd.join("srcs/a.py")).unwrap();
        let size = md.len();
        let mt = md
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dm = mtime_ns(&bd.join("srcs")).unwrap();
        let store_dir: StoreDir = "/nix/store".parse().unwrap();
        let enc = "/nix/store/00000000000000000000000000000000-a.py:srcs/a.py:";
        // Entry "good" matches the real fingerprint; "stale" lies about
        // the size, standing in for a source edit between rounds.
        fs::write(
            bd.join(FILE_NAME),
            format!(
                "{HEADER}\npy\tsrcs\t{dm}\t1\t{size}\t{mt}\t{enc}\n\
                 dir\tsrcs\t{dm}\t1\t{}\t{mt}\t{enc}\n",
                size + 1
            ),
        )
        .unwrap();
        init(store_dir, bd.clone());
        let hit = lookup("py", Path::new("srcs")).expect("matching entry must validate");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].build_path, PathBuf::from("srcs/a.py"));
        assert!(
            lookup("dir", Path::new("srcs")).is_none(),
            "mismatched fingerprint must drop the entry"
        );
        // record + flush appends a replayable line.
        record("py", Path::new("srcs"), &hit);
        flush().unwrap();
        let body = fs::read_to_string(bd.join(FILE_NAME)).unwrap();
        assert_eq!(body.matches("py\tsrcs").count(), 2);

        // NAR stamps: a saved entry whose store path no longer exists is
        // filtered at load (the failure direction is re-upload), and a
        // wrong header discards the file. The existence-positive needs a
        // real store object and is exercised by every warm restart.
        let ghost: StorePath = "/nix/store"
            .parse::<StoreDir>()
            .unwrap()
            .parse("/nix/store/11111111111111111111111111111111-ghost")
            .unwrap();
        save_nar_stamps(&[(PathBuf::from("/src/a.c"), 5, 7, ghost)]).unwrap();
        let body = fs::read_to_string(bd.join(NAR_FILE)).unwrap();
        assert!(body.starts_with(NAR_HEADER), "{body}");
        assert!(body.contains("/src/a.c\t5\t7\t/nix/store/"), "{body}");
        assert!(
            load_nar_stamps().is_empty(),
            "a GC'd store path must not seed the cache"
        );
        fs::write(bd.join(NAR_FILE), "wrong-header\njunk\n").unwrap();
        assert!(load_nar_stamps().is_empty());
        assert!(!bd.join(NAR_FILE).exists(), "bad header must discard the file");
        fs::remove_dir_all(&bd).unwrap();
    }
}
