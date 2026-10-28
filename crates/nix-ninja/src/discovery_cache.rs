//! Persistent disk cache for `discover_c_includes` answers.
//!
//! Every retry of a failed outer derivation restarts the driver cold: the
//! resolve memo and NAR stamps in `resolve_cache.rs` are off inside a nix
//! build by design, so each attempt re-walks every compile edge's include
//! closure and re-uploads it. Measured on qtwebengine, resolve runs into the
//! thousands of seconds with `dyn_discover` the dominant term. This module
//! persists the other end of that work: the `Discovered` set per edge, keyed
//! by edge identity plus a fingerprint of the seeds, in an explicit directory
//! the retry loop passes via `NIX_NINJA_DISCOVERY_CACHE_DIR`.
//!
//! Home is NEVER the build dir (the walk reads it into the implicit-input
//! blanket, so a growing cache file would move what every task declares)
//! and never the source tree (packaging checks fail on stray files). Unset,
//! the cache is off entirely, including inside a sandbox.
//!
//! Correctness posture mirrors `resolve_cache.rs`: the file carries a header
//! naming the format version plus this driver's `driver=` stamp, matched with
//! whole-line equality, so entries computed by another scanner never replay.
//! A hit revalidates every seed file (count, present count, total size, max
//! mtime); any drift is a miss and the edge recomputes. The failure direction
//! is re-discovery, never a stale input set. Store paths stay valid because
//! nothing GCs mid-campaign.
//!
//! Only no-defer answers are recorded. A deferred edge's answer depends on
//! which producers have run, which changes as the run proceeds; replaying a
//! deferral (or skipping one) would change scheduling. A no-defer
//! `Discovered` replays byte-identical emission: the task's Built inputs
//! still order it behind its producers, and producer outputs are
//! deterministic for a fixed outer derivation, so the bytes the replay
//! carries are the bytes a recompute would upload.

use crate::resolve_cache::driver_stamp;
use crate::task::Discovered;
use harmonia_store_path::{StoreDir, StorePath};
use nix_ninja_task::derived_file::DerivedFile;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

pub(crate) const FILE_NAME: &str = ".nix-ninja-discovery-cache.v1";
/// Bumped with any change to what an entry MEANS: key components, value
/// encoding, or validation. A mismatch discards the whole file.
const HEADER_BASE: &str = "nix-ninja-discovery-cache v1";
/// Separates encoded DerivedFiles and path lists within one line; never
/// appears in a store path or a build-relative path.
const SEP: char = '\x1f';

fn header() -> String {
    format!("{HEADER_BASE} driver={}", driver_stamp())
}

fn mtime_ns(p: &Path) -> Option<u128> {
    fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}

struct Fingerprint {
    total: usize,
    present: usize,
    sum_size: u64,
    max_mtime_ns: u128,
}

struct Entry {
    fp: Fingerprint,
    deps: Vec<String>,
    store_paths: Vec<String>,
    dotdot_dirs: Vec<String>,
}

fn fingerprint_seed(build_dir: &Path, seeds: &[PathBuf]) -> Fingerprint {
    let mut present = 0usize;
    let mut sum_size = 0u64;
    let mut max_mtime_ns = 0u128;
    for rel in seeds {
        let Ok(md) = fs::metadata(build_dir.join(rel)) else {
            continue;
        };
        present += 1;
        sum_size += md.len();
        if let Ok(m) = md.modified() {
            if let Ok(d) = m.duration_since(UNIX_EPOCH) {
                max_mtime_ns = max_mtime_ns.max(d.as_nanos());
            }
        }
    }
    Fingerprint {
        total: seeds.len(),
        present,
        sum_size,
        max_mtime_ns,
    }
}

/// The cache key: edge identity plus stable seed identity. Sections joined
/// with \x1e, items with \x1f; the hex digest is the persisted key.
///
/// Sections: cmdline; seeds as given; depfile state (absent, unreadable, or
/// path plus content hash - a depfile read-back replaces the scan, so its
/// bytes are part of the identity); sorted virtual map; sorted carried
/// trees; build dir; manifest size and mtime (binds entries to the exact
/// build.ninja so a shared dir across builds goes cold, never stale).
fn cache_key(
    build_dir: &Path,
    cmdline: &str,
    seeds: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
    depfile: Option<&Path>,
    carried_trees: &[PathBuf],
) -> String {
    let mut h = Sha256::new();
    let mut section = |s: &str| {
        h.update(s.as_bytes());
        h.update([0x1e]);
    };
    section(cmdline);
    section(
        &seeds
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\x1f"),
    );
    let dep_state = match depfile {
        None => "none".to_string(),
        Some(d) => match fs::read(build_dir.join(d)) {
            Ok(bytes) => format!("{}:{:x}", d.display(), Sha256::digest(&bytes)),
            Err(_) => format!("{}:unreadable", d.display()),
        },
    };
    section(&dep_state);
    let mut virt: Vec<String> = virtual_paths
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{}=>{}", k.display(), v.display()))
                .collect()
        })
        .unwrap_or_default();
    virt.sort();
    section(&virt.join("\x1f"));
    let mut carried: Vec<String> = carried_trees
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    carried.sort();
    section(&carried.join("\x1f"));
    section(&build_dir.to_string_lossy());
    let manifest = build_dir.join("build.ninja");
    section(&match fs::metadata(&manifest) {
        Ok(md) => format!("{}:{}", md.len(), mtime_ns(&manifest).unwrap_or(0)),
        Err(_) => "absent".to_string(),
    });
    format!("{:x}", h.finalize())
}

/// Parse a persisted cache body, or `None` when its first line is not
/// EXACTLY this driver's header. Whole-line equality is the gate: a file
/// written by a different driver build carries a different `driver=` stamp,
/// and anything short of equality replays answers computed by another
/// scanner. A loosened gate (prefix match, missing stamp) is rejected by
/// the unit tests below, not by inspection.
fn parse_persisted(body: &str, expected: &str) -> Option<HashMap<String, Entry>> {
    let mut lines = body.lines();
    if lines.next() != Some(expected) {
        return None;
    }
    let mut out = HashMap::new();
    for line in lines {
        let mut f = line.split('\t');
        let (
            Some(key),
            Some(total),
            Some(present),
            Some(sum),
            Some(maxm),
            Some(deps),
            Some(store),
            Some(dots),
        ) = (
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
        )
        else {
            continue;
        };
        let (Ok(total), Ok(present), Ok(sum_size), Ok(max_mtime_ns)) =
            (total.parse(), present.parse(), sum.parse(), maxm.parse())
        else {
            continue;
        };
        let split = |s: &str| {
            if s.is_empty() {
                Vec::new()
            } else {
                s.split(SEP).map(str::to_string).collect()
            }
        };
        // Last write wins, same as the in-memory memos.
        out.insert(
            key.to_string(),
            Entry {
                fp: Fingerprint {
                    total,
                    present,
                    sum_size,
                    max_mtime_ns,
                },
                deps: split(deps),
                store_paths: split(store),
                dotdot_dirs: split(dots),
            },
        );
    }
    Some(out)
}

pub(crate) struct DiscoveryCache {
    store_dir: StoreDir,
    build_dir: PathBuf,
    path: PathBuf,
    unvalidated: Mutex<HashMap<String, Entry>>,
    pending: Mutex<Vec<String>>,
}

impl DiscoveryCache {
    fn new(store_dir: StoreDir, build_dir: PathBuf, dir: PathBuf) -> Option<Self> {
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let path = dir.join(FILE_NAME);
        let mut unvalidated = HashMap::new();
        if let Ok(body) = fs::read_to_string(&path) {
            match parse_persisted(&body, &header()) {
                Some(m) => unvalidated = m,
                None => {
                    // Remove, don't just ignore: flush() writes the header
                    // only when the file is absent, so appending under a
                    // stale header strands every entry this run banks.
                    if fs::remove_file(&path).is_err() {
                        eprintln!(
                            "nix-ninja: discovery cache {} has a stale header and could not be \
                             removed; persistence is disabled for this run rather than appending under it",
                            path.display()
                        );
                        return None;
                    }
                    eprintln!(
                        "nix-ninja: discovery cache {} had a different version header; removed",
                        path.display()
                    );
                }
            }
        }
        // ALWAYS, including zero: zero is the state an operator most needs
        // to know, and printing nothing made it indistinguishable from the
        // cache being disabled or this code never running.
        let loaded = unvalidated.len();
        eprintln!(
            "nix-ninja: discovery cache {}: {loaded} edge entries loaded{}",
            path.display(),
            if loaded == 0 {
                ", so this run discovers cold"
            } else {
                ""
            }
        );
        Some(DiscoveryCache {
            store_dir,
            build_dir,
            path,
            unvalidated: Mutex::new(unvalidated),
            pending: Mutex::new(Vec::new()),
        })
    }

    fn lookup(
        &self,
        cmdline: &str,
        seeds: &[PathBuf],
        virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
        depfile: Option<&Path>,
        carried_trees: &[PathBuf],
    ) -> Option<Discovered> {
        let key = cache_key(
            &self.build_dir,
            cmdline,
            seeds,
            virtual_paths,
            depfile,
            carried_trees,
        );
        let entry = self.unvalidated.lock().unwrap().remove(&key)?;
        let fp = fingerprint_seed(&self.build_dir, seeds);
        if fp.total != entry.fp.total
            || fp.present != entry.fp.present
            || fp.sum_size != entry.fp.sum_size
            || fp.max_mtime_ns != entry.fp.max_mtime_ns
        {
            return None;
        }
        let deps: Vec<_> = entry
            .deps
            .iter()
            .map(|e| DerivedFile::from_encoded(&self.store_dir, e).ok())
            .collect::<Option<_>>()?;
        let mut store_paths = Vec::with_capacity(entry.store_paths.len());
        for s in &entry.store_paths {
            store_paths.push(StorePath::from_base_path(s).ok()?);
        }
        let dotdot_dirs = entry.dotdot_dirs.iter().map(PathBuf::from).collect();
        crate::task::DISCOVERY_CACHE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(Discovered {
            deps,
            store_paths,
            dotdot_dirs,
        })
    }

    fn record(
        &self,
        cmdline: &str,
        seeds: &[PathBuf],
        virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
        depfile: Option<&Path>,
        carried_trees: &[PathBuf],
        discovered: &Discovered,
    ) {
        let key = cache_key(
            &self.build_dir,
            cmdline,
            seeds,
            virtual_paths,
            depfile,
            carried_trees,
        );
        let fp = fingerprint_seed(&self.build_dir, seeds);
        let entry = Entry {
            fp,
            deps: discovered
                .deps
                .iter()
                .map(|f| f.to_encoded(&self.store_dir))
                .collect(),
            store_paths: discovered
                .store_paths
                .iter()
                .map(StorePath::to_string)
                .collect(),
            dotdot_dirs: discovered
                .dotdot_dirs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        };
        let line = format!(
            "{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.fp.total,
            entry.fp.present,
            entry.fp.sum_size,
            entry.fp.max_mtime_ns,
            entry.deps.join(&SEP.to_string()),
            entry.store_paths.join(&SEP.to_string()),
            entry.dotdot_dirs.join(&SEP.to_string()),
        );
        // Insert, not just queue: a deferred edge is rediscovered later in
        // the SAME run, and without this the record is invisible until the
        // next attempt loads the flushed file.
        self.unvalidated.lock().unwrap().insert(key, entry);
        self.pending.lock().unwrap().push(line);
    }

    fn flush(&self) -> anyhow::Result<()> {
        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        if file.metadata()?.len() == 0 {
            writeln!(file, "{}", header())?;
        }
        for line in pending.drain(..) {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

static CACHE: OnceLock<Option<DiscoveryCache>> = OnceLock::new();

/// Call once from Runner::new. The cache lives only where
/// `NIX_NINJA_DISCOVERY_CACHE_DIR` names: unset, discovery always recomputes,
/// including inside a sandbox.
pub fn init(store_dir: StoreDir, build_dir: PathBuf) {
    CACHE.get_or_init(|| {
        let dir: PathBuf = std::env::var_os("NIX_NINJA_DISCOVERY_CACHE_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)?;
        DiscoveryCache::new(store_dir, build_dir, dir)
    });
}

/// A validated hit, or None (miss, invalidated, or cache disabled).
pub fn lookup(
    cmdline: &str,
    seeds: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
    depfile: Option<&Path>,
    carried_trees: &[PathBuf],
) -> Option<Discovered> {
    let cache = CACHE.get()?.as_ref()?;
    let hit = cache.lookup(cmdline, seeds, virtual_paths, depfile, carried_trees);
    crate::task::DISCOVERY_CACHE_LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    hit
}

/// Record a freshly computed no-defer answer for the next attempt. Only the
/// no-defer half is recorded; see the module docs.
pub fn record(
    cmdline: &str,
    seeds: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
    depfile: Option<&Path>,
    carried_trees: &[PathBuf],
    discovered: &Discovered,
) {
    let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) else {
        return;
    };
    cache.record(
        cmdline,
        seeds,
        virtual_paths,
        depfile,
        carried_trees,
        discovered,
    );
}

/// Append entries computed since the last tick; a killed driver loses at
/// most one tick's worth.
pub fn flush() -> anyhow::Result<()> {
    if let Some(cache) = CACHE.get().and_then(|c| c.as_ref()) {
        cache.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache(dir: &Path) -> DiscoveryCache {
        let store_dir: StoreDir = "/nix/store".parse().unwrap();
        DiscoveryCache::new(store_dir, dir.to_path_buf(), dir.join("dc")).unwrap()
    }

    fn write_seed(dir: &Path, name: &str, body: &str) -> PathBuf {
        let rel = PathBuf::from(name);
        fs::write(dir.join(&rel), body).unwrap();
        rel
    }

    #[test]
    fn a_foreign_header_is_rejected_not_parsed() {
        // THE EQUALITY IS THE GATE. A prefix match would accept a file
        // written by an older format, and a missing driver stamp would accept
        // any driver's entries; both replay answers another scanner computed.
        let good = format!("{HEADER_BASE} driver=abc123");
        let body = format!("{good}\nk\t1\t1\t5\t0\t\t\t\n");
        assert!(parse_persisted(&body, &good).unwrap().contains_key("k"));
        let prefix = format!("{HEADER_BASE} driver=abc123EXTRA");
        assert!(parse_persisted(&format!("{prefix}\nk\t1\t1\t5\t0\t\t\t\n"), &good).is_none());
        let nostamp = HEADER_BASE.to_string();
        assert!(parse_persisted(&format!("{nostamp}\nk\t1\t1\t5\t0\t\t\t\n"), &good).is_none());
        let truncated = format!("{HEADER_BASE} driver=abc12");
        assert!(parse_persisted(&format!("{truncated}\nk\t1\t1\t5\t0\t\t\t\n"), &good).is_none());
    }

    #[test]
    fn the_key_moves_with_every_identity_section() {
        let dir = std::env::temp_dir().join("nn-dc-key");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_seed(&dir, "build.ninja", "x");
        let seeds = vec![PathBuf::from("a.c")];
        let base = cache_key(&dir, "cc a.c", &seeds, None, None, &[]);
        assert_ne!(base, cache_key(&dir, "cc -O2 a.c", &seeds, None, None, &[]));
        assert_ne!(
            base,
            cache_key(&dir, "cc a.c", &[PathBuf::from("b.c")], None, None, &[])
        );
        let mut virt = HashMap::new();
        virt.insert(PathBuf::from("h.h"), PathBuf::from("gen/h.h"));
        assert_ne!(
            base,
            cache_key(&dir, "cc a.c", &seeds, Some(&virt), None, &[])
        );
        assert_ne!(
            base,
            cache_key(&dir, "cc a.c", &seeds, None, None, &[PathBuf::from("tree")],)
        );
        // Same inputs, same key.
        assert_eq!(base, cache_key(&dir, "cc a.c", &seeds, None, None, &[]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recorded_answer_replays_identically() {
        let dir = std::env::temp_dir().join("nn-dc-replay");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_seed(&dir, "build.ninja", "x");
        let cache = test_cache(&dir);
        let seeds = vec![write_seed(&dir, "a.c", "int main(){}\n")];
        let found = Discovered {
            deps: vec![],
            store_paths: vec![],
            dotdot_dirs: vec![PathBuf::from("..")],
        };
        cache.record("cc a.c", &seeds, None, None, &[], &found);
        let hit = cache.lookup("cc a.c", &seeds, None, None, &[]).unwrap();
        assert_eq!(hit.dotdot_dirs, vec![PathBuf::from("..")]);
        assert!(hit.deps.is_empty() && hit.store_paths.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_seed_is_a_miss_not_a_stale_replay() {
        let dir = std::env::temp_dir().join("nn-dc-stale");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_seed(&dir, "build.ninja", "x");
        let cache = test_cache(&dir);
        let seeds = vec![write_seed(&dir, "a.c", "int x;\n")];
        cache.record(
            "cc a.c",
            &seeds,
            None,
            None,
            &[],
            &Discovered {
                deps: vec![],
                store_paths: vec![],
                dotdot_dirs: vec![],
            },
        );
        // Same byte length, different content, newer mtime: the max-mtime
        // half of the fingerprint drops it.
        write_seed(&dir, "a.c", "int y;\n");
        filetime_set(&dir.join("a.c"));
        assert!(cache.lookup("cc a.c", &seeds, None, None, &[]).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn filetime_set(p: &Path) {
        // Bump the mtime without changing the size, so the test exercises the
        // mtime half rather than the size half.
        use std::os::unix::fs::MetadataExt;
        let t = fs::metadata(p).unwrap().mtime() + 2;
        let ft = std::time::UNIX_EPOCH + std::time::Duration::from_secs(t as u64);
        filetime_set_mtime(p, ft);
    }

    #[cfg(unix)]
    fn filetime_set_mtime(p: &Path, t: std::time::SystemTime) {
        let _ = std::process::Command::new("touch")
            .arg("-d")
            .arg(format!(
                "@{}",
                t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
            ))
            .arg(p)
            .status();
    }
}
