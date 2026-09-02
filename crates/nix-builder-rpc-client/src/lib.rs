//! Sync client for the Nix daemon's worker protocol, backed by
//! `harmonia_store_remote::pool::ConnectionPool` for concurrent acquisition
//! across worker threads. Inside a `builder-rpc-v0` sandbox
//! (NixOS/nix#15793), the daemon socket is exposed via `$NIX_REMOTE` and
//! only a small allowlist is permitted: `Add{ToStore,ToStoreNar,TextToStore}`
//! plus `SubmitOutput`. Outside the sandbox the same client talks to the
//! standard daemon socket.

pub mod aterm;

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harmonia_protocol::daemon_wire::types2::{BuildMode, BuildResultInner};
use harmonia_protocol::store_path::StoreDir;
use harmonia_protocol::types::{DaemonError, DaemonErrorKind, DaemonStore};
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::{Derivation, DerivationInputs};
use harmonia_store_derivation::derived_path::{
    DerivedPath, OutputName, OutputSpec, SingleDerivedPath,
};
use harmonia_store_path::{StorePath, StorePathSet};
use harmonia_store_remote::{ConnectionPool, PoolConfig};
use tokio::io::BufReader;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// Env var the daemon sets inside a `builder-rpc-v0` sandbox.
pub const SOCKET_ENV: &str = "NIX_REMOTE";

/// Fallback for when `$NIX_REMOTE` is unset — Nix's standard daemon socket path.
const DEFAULT_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// Waits between connect attempts, in seconds. A connect failure means the
/// daemon is DOWN rather than busy, and the commonest cause is a restart:
/// s6 stops it, the new one starts, and the socket is refused in between.
/// A flat 10s x 3 gave up in half a minute and lost a 45-minute round.
const CONNECT_BACKOFF_S: [u64; 6] = [5, 10, 20, 30, 60, 60];

/// Connect attempts before surrendering. One per entry in the ladder above,
/// so the two cannot drift: the last entry repeats if this were ever raised
/// alone, which is a silent behaviour change, hence the assertion in tests.
const CONNECT_RETRIES: u32 = CONNECT_BACKOFF_S.len() as u32;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("daemon error: {0}")]
    Daemon(#[from] DaemonError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("$NIX_REMOTE has unsupported scheme: {0}")]
    UnsupportedRemote(String),
    #[error("No Nix daemon running, if you are on a single-user Nix install run `nix-daemon`")]
    NoDaemon,
    #[error("nar encode: {0}")]
    Nar(String),
    // "string is too long" from the daemon names neither the string nor
    // the call; this wrapper names the object being added so the failing
    // upload is identifiable from the build log (libconfig, 2026-08-23).
    #[error("add_to_store of {name:?} ({detail}): {source}")]
    AddToStore {
        name: String,
        detail: String,
        #[source]
        source: Box<Error>,
    },
    #[error("build of {path} failed: {error_msg}")]
    BuildFailed { path: String, error_msg: String },
    #[error("daemon returned no build result for {0}")]
    MissingBuildResult(String),
    // The variant name leads the Display text deliberately: the shell
    // monitors grep the LOG for "DaemonStalled", and the log only ever
    // sees this error through Display - a message without the name is a
    // stall the monitoring cannot match.
    #[error(
        "DaemonStalled: no useful daemon reply through {attempts} stall attempts \
         and {connect_failures} connect failures (final allowance \
         {last_allowance_s}s); daemon-side wedge"
    )]
    DaemonStalled {
        attempts: u32,
        connect_failures: u32,
        last_allowance_s: u64,
    },
}

/// The OTHER failures in the same batch, named.
///
/// A realise is a batch and the scheduler keeps going, so a task reporting
/// "1 dependency failed" is usually accompanied in the same reply by the
/// result that says WHICH one. Reporting only the target's own message
/// discarded that, leaving a failure whose cause is named nowhere and no
/// derivation to run `nix log` against (libssh and onetbb, round 14).
///
/// Capped, because a wide batch can fail widely; the count says what the cap
/// hid, so a truncated list never reads as a complete one.
fn co_failure_summary(failures: &[String], own: &str) -> String {
    const SHOWN: usize = 5;
    let rest: Vec<&String> = failures.iter().filter(|f| !f.starts_with(own)).collect();
    if rest.is_empty() {
        return String::new();
    }
    let head: Vec<String> = rest.iter().take(SHOWN).map(|f| f.to_string()).collect();
    let more = rest.len().saturating_sub(head.len());
    let tail = if more > 0 {
        format!(" (and {more} more in the same batch)")
    } else {
        String::new()
    };
    format!("; also failed in this batch: {}{tail}", head.join(" | "))
}

#[cfg(test)]
mod co_failure_summary_tests {
    use super::co_failure_summary;

    /// THE CASE IT EXISTS FOR. The target says only that a dependency failed;
    /// the dependency's own result is in the same reply and names itself.
    #[test]
    fn a_siblings_failure_is_named() {
        let f = vec![
            "/nix/store/aaa-target.drv: 1 dependency failed".to_string(),
            "/nix/store/bbb-dep.drv: builder failed with exit code 1".to_string(),
        ];
        let s = co_failure_summary(&f, "/nix/store/aaa-target.drv");
        assert!(s.contains("bbb-dep.drv"), "{s}");
        assert!(!s.contains("aaa-target.drv"), "{s}");
    }

    /// A lone failure adds nothing, so a task that simply failed reads exactly
    /// as it did before.
    #[test]
    fn the_only_failure_produces_no_tail() {
        let f = vec!["/nix/store/aaa-target.drv: boom".to_string()];
        assert_eq!(co_failure_summary(&f, "/nix/store/aaa-target.drv"), "");
        assert_eq!(co_failure_summary(&[], "/nix/store/aaa-target.drv"), "");
    }

    /// THE CAP MUST ANNOUNCE ITSELF. A truncated list that does not say it was
    /// truncated is read as the whole set, which is the shape that has
    /// produced a wrong count in this project more than once.
    #[test]
    fn truncation_states_the_remainder() {
        let f: Vec<String> = (0..9)
            .map(|i| format!("/nix/store/d{i}.drv: boom"))
            .collect();
        let s = co_failure_summary(&f, "/nix/store/none");
        assert!(s.contains("and 4 more in the same batch"), "{s}");
        assert_eq!(s.matches(".drv").count(), 5, "{s}");
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Watchdog allowance for the Nth stall retry: 300s, 1200s, 4800s, then
/// 4800s again for the final attempt. The `.min(2)` cap is load-bearing:
/// without it a large attempt count shifts past the u64 width.
fn stall_allowance_s(stall_attempts: u32) -> u64 {
    300u64 << (2 * stall_attempts.min(2))
}

#[cfg(test)]
mod watchdog_policy_tests {
    use super::stall_allowance_s;

    // The shell monitors grep the LOG for these substrings, and the log
    // only ever sees an error through Display - a pattern verified
    // against the SOURCE (which names the variant) rather than an
    // emitted line matched nothing for a whole campaign. So: format the
    // real error, assert the monitored pattern hits.
    #[test]
    fn monitored_patterns_appear_in_display_output() {
        let e = super::Error::DaemonStalled {
            attempts: 4,
            connect_failures: 1,
            last_allowance_s: 4800,
        };
        let line = e.to_string();
        assert!(
            line.contains("DaemonStalled"),
            "monitor grep would miss: {line}"
        );
        assert!(line.contains("daemon-side wedge"));
    }

    #[test]
    fn allowance_schedule_is_pinned() {
        assert_eq!(stall_allowance_s(0), 300);
        assert_eq!(stall_allowance_s(1), 1200);
        assert_eq!(stall_allowance_s(2), 4800);
        // The cap: every later attempt stays at 4800, and in particular
        // an attempt count >= 32 must not shift into nonsense.
        assert_eq!(stall_allowance_s(3), 4800);
        assert_eq!(stall_allowance_s(64), 4800);
    }
}

pub struct BuilderRpcClient {
    /// Multi-thread so the pool's RAII drop tasks can complete asynchronously.
    runtime: Runtime,
    pool: ConnectionPool,
    /// Whether or not nix-ninja is running within a derivation
    /// Needed since scanning is not available outside derivations
    in_drv: bool,

    /// ATerm bytes kept because builder-rpc-v0 does not materialize uploaded .drv files in the sandbox.
    uploaded_drvs: Mutex<HashMap<StorePath, Vec<u8>>>,

    /// Derived path (as displayed) -> the store path it realised to.
    ///
    /// Outside a sandbox every gcc task realises its own built inputs before
    /// the local header scan, and neighbouring tasks share nearly all of
    /// them - the generated-header set of one component is an input to every
    /// TU in it. Without this the driver pays a daemon round trip per task
    /// for paths it realised seconds earlier, on the SERIAL resolve loop, and
    /// that repetition was the whole gap between round 86's 142 s of counted
    /// resolve time and its ~43 minutes of wall clock.
    ///
    /// Sound because a realisation is immutable: a derived path resolves to
    /// one store path, and CA derivations make that a function of content.
    /// The only way a hit goes stale is the store path being collected, so a
    /// hit is confirmed by existence before it is used - one stat against one
    /// round trip.
    /// The bool is EXISTENCE ALREADY CONFIRMED. See `build_paths`: the
    /// check that guards against a collected path used to run on every
    /// hit, which made it O(paths asked) rather than O(paths).
    realised: Mutex<HashMap<String, (StorePath, bool)>>,

    /// Canonical path -> (size, mtime_ns, store path) for NAR uploads.
    ///
    /// Dependency discovery uploads a store object per discovered include,
    /// and headers are shared across a component's whole TU set, so the same
    /// file was NAR-encoded and sent once per TU that included it. This is
    /// the same defect the realise memo fixed, on the other RPC.
    ///
    /// Build-directory files are MUTABLE, unlike store paths, so the key
    /// carries size and mtime and a changed file is a miss. The failure
    /// direction is re-upload, never a stale store path - the same posture
    /// resolve_cache states for its own entries.
    ///
    /// Two threads racing the same fresh path both upload; the result is
    /// content-addressed so that is wasted work rather than a wrong answer,
    /// and holding the lock across an upload would serialize every upload in
    /// the round.
    nar_uploads: Mutex<HashMap<PathBuf, (u64, u128, StorePath)>>,
}

// Wall-clock attribution for the realise RPC.
//
// The driver timed four sub-phases of task RESOLUTION and nothing at all
// around the daemon round trip, so a round that spent 85% of its wall clock
// inside one `build_paths` reported 129 s of "total resolve time" and looked
// idle. Three rounds were then tuned against memory, which was never the
// bound. A Drop guard rather than a timed block because this function has
// several early returns and an untimed one would understate the very case
// worth catching.

/// `(asked, sent)` realise paths so far: what callers requested, and what
/// actually reached the daemon. Reported by the driver's progress tick,
/// because a memo whose saving is never printed is a claim nobody can check.
pub fn realise_stats() -> (u64, u64) {
    (
        REALISE_ASKED.load(std::sync::atomic::Ordering::Relaxed),
        REALISE_SENT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Sort miss slots and return them alongside the same order, so the request
/// vector rebuilt from them lines up with the daemon's reply.
///
/// A demoted hit is appended after the original misses, so `miss_slots` is no
/// longer ascending; `merge_hits_and_misses` pairs `miss_slots[k]` with
/// `built[k]` and a disagreement returns a real store path for the WRONG
/// input, which fails three layers away in a compile.
///
/// `len` is the caller's path count: a slot outside it is a bug here, not
/// upstream, and is dropped rather than allowed to index out of bounds.
///
/// Returns ONE vector, not two. It returned `(v.clone(), v)` - the same order
/// twice - which made the caller's destructuring read as though the slot order
/// and the request order could differ, which is exactly the divergence this
/// function exists to prevent. A signature that suggests two orders invites
/// somebody to make them two. Reported by the specification session.
fn reorder_misses(miss_slots: &[usize], len: usize) -> Vec<usize> {
    let mut v: Vec<usize> = miss_slots.iter().copied().filter(|&i| i < len).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Put freshly realised paths back into the slots their requests came from.
///
/// Extracted and generic so it can be tested: the daemon-facing half of the
/// memo needs a live daemon, but the half that can silently corrupt an answer
/// does not. A misaligned slot returns the WRONG store path for a real input
/// and every later failure points somewhere else - the caller symlinks it and
/// a compile fails on a missing header three layers away.
///
/// Panics if a slot is unfilled or the miss count disagrees with the built
/// count. Both are impossible from the one caller and both are silent
/// corruption if they ever stop being.
fn merge_hits_and_misses<T>(
    mut slots: Vec<Option<T>>,
    miss_slots: Vec<usize>,
    built: Vec<T>,
) -> Vec<T> {
    assert_eq!(
        miss_slots.len(),
        built.len(),
        "realise returned {} paths for {} requests",
        built.len(),
        miss_slots.len(),
    );
    for (slot, value) in miss_slots.into_iter().zip(built) {
        slots[slot] = Some(value);
    }
    slots
        .into_iter()
        .map(|o| o.expect("every slot is filled by a hit or by a miss"))
        .collect()
}

/// Paths the driver asked to realise, and the subset that reached the daemon.
/// The ratio is the memo's validation number and the only honest way to state
/// its benefit; a hit count alone says nothing about what was avoided.
static REALISE_ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REALISE_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static BUILD_PATHS_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BUILD_PATHS_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A single realise slower than this names itself immediately, with the path
/// count, rather than waiting for a total nobody reads mid-round.
const SLOW_REALISE_MS: u64 = 30_000;
// The band this threshold has to sit in, checked when the crate COMPILES
// rather than when a test runs. It was written as a #[test] full of literal
// comparisons against the constant, which clippy reads as an assertion with a
// constant value and is right to: nothing in it could fail at run time, so it
// read as a behavioural test while being a compile-time fact. A round that
// spends minutes in one realise must trip it; a healthy sub-second realise
// must not.
const _: () = assert!(SLOW_REALISE_MS < 735_000);
const _: () = assert!(SLOW_REALISE_MS > 800);

struct RealiseTimer {
    started: std::time::Instant,
    paths: usize,
}

impl Drop for RealiseTimer {
    fn drop(&mut self) {
        let ms = self.started.elapsed().as_millis() as u64;
        let total = BUILD_PATHS_MS.fetch_add(ms, std::sync::atomic::Ordering::Relaxed) + ms;
        let calls = BUILD_PATHS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if ms >= SLOW_REALISE_MS {
            eprintln!(
                "nix-ninja: SLOW REALISE {} s for {} derived path(s) \
                 (realise total {} s over {} call(s))",
                ms / 1000,
                self.paths,
                total / 1000,
                calls,
            );
        }
    }
}

/// Worker threads for the RPC runtime, sized to the work it can actually have.
///
/// Tokio's default is one worker per hardware thread, which is 24 here. Every
/// task this runtime runs is an RPC on the connection pool, so at most
/// `pool_max` of them can be in flight and the rest of the workers can never
/// hold work. Measured on a live round: a driver spawned for a SINGLE compile
/// carried 30 to 32 threads at a cumulative 0.30 to 0.43 CPU-seconds, never
/// one core's worth across all of them.
///
/// The cost is not idle threads burning CPU - it is pool CONSTRUCTION, paid
/// once per driver and never amortized, because a compiler-route driver lives
/// 0.4 to 4 seconds and there is one per translation unit.
///
/// `max(1)` because tokio panics on zero workers, and a caller asking for no
/// connections still needs a runtime that can run the request that says so.
fn rpc_worker_threads(pool_max: usize) -> usize {
    pool_max.max(1)
}

#[cfg(test)]
mod rpc_worker_threads_tests {
    use super::rpc_worker_threads;

    /// The runtime must not be wider than the pool it serves, and must never
    /// be zero - tokio panics on a zero-worker runtime, so the floor is the
    /// half of this that cannot be left to a comment.
    #[test]
    fn tracks_the_pool_and_never_reaches_zero() {
        assert_eq!(rpc_worker_threads(0), 1);
        assert_eq!(rpc_worker_threads(1), 1);
        assert_eq!(rpc_worker_threads(6), 6);
        assert_eq!(rpc_worker_threads(24), 24);
    }

    /// A one-edge driver is the case this exists for: it must not open a
    /// worker per hardware thread. Asserted as a RELATIONSHIP rather than a
    /// literal, so it still means something on a machine of any width.
    #[test]
    fn a_single_connection_does_not_size_to_the_machine() {
        let machine = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        if machine > 1 {
            assert!(
                rpc_worker_threads(1) < machine,
                "a one-connection driver must not build a machine-wide runtime"
            );
        }
    }
}

impl BuilderRpcClient {
    /// Connect to `$NIX_REMOTE` if set, otherwise the standard daemon
    pub fn connect_from_env(pool_max: Option<usize>) -> Result<Self> {
        let path = match std::env::var(SOCKET_ENV) {
            Ok(remote) => parse_unix_remote(&remote)?,
            Err(_) => PathBuf::from(DEFAULT_DAEMON_SOCKET),
        };
        if !path.exists() {
            return Err(Error::NoDaemon);
        }

        // There is no simple OS flag to tell if we are running in a derivation,
        // but NIX_BUILD_TOP is always set by the nix daemon before building
        // and has no purpose outside a nix derivation.
        let in_drv = std::env::var_os("NIX_BUILD_TOP").is_some();

        Self::connect_unix_sized(
            &path,
            in_drv,
            pool_max.unwrap_or(PoolConfig::default().max_size),
        )
    }

    pub fn connect_unix(path: &Path, in_drv: bool) -> Result<Self> {
        Self::connect_unix_sized(path, in_drv, PoolConfig::default().max_size)
    }

    /// `pool_max` connections to the daemon. Each in-flight build occupies
    /// one, so this is a concurrency bound in its own right and it must not
    /// be left to sit below `-j`: `PoolConfig::default()` is
    /// `available_parallelism() + 1`, a number nobody chose, and a `-j`
    /// above it silently becomes that instead. Measured 2026-08-20 on a
    /// 24-thread machine: the default is 25, so every `-j` from 26 up ran
    /// at 25 with no message.
    pub fn connect_unix_sized(path: &Path, in_drv: bool, pool_max: usize) -> Result<Self> {
        let runtime = RuntimeBuilder::new_multi_thread()
            .worker_threads(rpc_worker_threads(pool_max))
            .enable_all()
            .build()?;
        let pool = ConnectionPool::new(
            path,
            PoolConfig {
                max_size: pool_max.max(1),
                ..PoolConfig::default()
            },
        );
        Ok(Self {
            runtime,
            pool,
            in_drv,
            uploaded_drvs: Default::default(),
            realised: Default::default(),
            nar_uploads: Default::default(),
        })
    }

    /// The ATerm bytes of an uploaded derivation.
    ///
    /// Inside a derivation the uploaded .drv is not materialized, so the only
    /// copy is the one `add_drv_to_store` kept. OUTSIDE one the store path is
    /// a readable file, so the bytes are read back on demand and nothing is
    /// retained - see `add_drv_to_store` for why that matters.
    pub fn clone_drv(&self, store_dir: &StoreDir, store_path: &StorePath) -> Option<Vec<u8>> {
        if let Some(bytes) = self.uploaded_drvs.lock().unwrap().get(store_path) {
            return Some(bytes.clone());
        }
        if self.in_drv {
            return None;
        }
        std::fs::read(store_path.to_absolute_path(store_dir)).ok()
    }

    // Serialise a derivation and add it to the store.
    // Should be preferred to `add_to_store_text` for derivations,
    pub fn add_drv_to_store(&self, store_dir: &StoreDir, drv: &Derivation) -> Result<StorePath> {
        // harmonia 4ec1435 split a derivation's inputs into the "full"
        // representation, DerivationInputs { srcs, drvs }, and the ATerm
        // printer now takes that rather than the flat BTreeSet we hold.
        // map_inputs is harmonia's own conversion point and the From impl
        // beside it does the splitting, so this is a shape change at the
        // boundary and not a semantic one.
        let full = drv.clone().map_inputs(|i| DerivationInputs::from(&i));
        let bytes = aterm::print_derivation_aterm(store_dir, &full);
        let refs: StorePathSet = drv.inputs.iter().map(|p| p.root_path().clone()).collect();
        let name = format!("{}.drv", drv.name);
        let info = self.runtime.block_on(async {
            let mut guard = self.pool.acquire().await?;
            let source = BufReader::new(Cursor::new(bytes.clone()));
            guard
                .execute(|client| {
                    client.add_ca_to_store(
                        &name,
                        ContentAddressMethodAlgorithm::Text,
                        &refs,
                        false,
                        source,
                    )
                })
                .await
        })?;

        // Retained ONLY inside a derivation, where builder-rpc-v0 does not
        // materialize the uploaded .drv and this is the sole copy. Outside
        // one the file is on disk and clone_drv reads it back, so retaining
        // is cost with no reader: both call sites are sandbox-only and
        // neither runs on the full-graph path this campaign uses.
        //
        // NO SIZE FIGURE HERE ON PURPOSE. The first version of this comment
        // claimed ~6 GiB, extrapolated from a 384 KiB mean over every
        // ninja-build.drv in the store, and the next progress tick falsified
        // it: driver RSS fell from 13,335 to 4,568 MiB between two 500-task
        // windows, and a monotonic map cannot exceed a resident size that
        // drops below it. The saving is real and unmeasured; measure it by
        // comparing RSS at equal task counts across a round with and without
        // this branch, which is the only reading that isolates it.
        if self.in_drv {
            self.uploaded_drvs
                .lock()
                .unwrap()
                .insert(info.path.clone(), bytes);
        }
        Ok(info.path)
    }

    /// Add bytes as a text-CA store object.
    /// Use for small files, but never for derivations.
    /// Derivations have different reference scanning logic, implemented in the
    /// `add_drv_to_store` function
    pub fn add_to_store_text(&self, name: &str, bytes: &[u8]) -> Result<StorePath> {
        let info = self
            .runtime
            .block_on(async {
                let mut guard = self.pool.acquire().await?;
                let source = BufReader::new(Cursor::new(bytes));
                if self.in_drv {
                    guard
                        .execute(|client| {
                            client.add_to_store_scanning(
                                name,
                                ContentAddressMethodAlgorithm::Text,
                                source,
                            )
                        })
                        .await
                } else {
                    // Unfortunately outside of a derivation we do not have a good
                    // idea of possible paths for which we can scan.
                    // A trivial "scan for all paths" implementation would include nonexistent paths
                    // from documentation and fail on important projects, e.g. nix.
                    // An empty reference set is identical to the fallback `nix store add` case
                    // and preferable to no running outside a derivation at all.
                    let refs = Default::default();
                    guard
                        .execute(|client| {
                            client.add_ca_to_store(
                                name,
                                ContentAddressMethodAlgorithm::Text,
                                &refs,
                                false,
                                source,
                            )
                        })
                        .await
                }
            })
            .map_err(|e| Error::AddToStore {
                name: name.to_string(),
                detail: format!("text, {} bytes", bytes.len()),
                source: Box::new(Error::from(e)),
            })?;
        Ok(info.path)
    }

    /// NAR a filesystem path then upload it as a recursive-CA (NAR-hashed)
    /// store object.
    /// NAR and upload `path`, skipping the work if `key` is unchanged since
    /// the last upload of it.
    ///
    /// `key` is the file whose identity is being cached - the canonical path -
    /// while `path` may be a rewritten copy (a shebang patch), which is a
    /// pure function of that file's content and so is covered by the same
    /// size-and-mtime check.
    pub fn add_to_store_nar_cached(
        &self,
        name: &str,
        path: &Path,
        key: &Path,
    ) -> Result<StorePath> {
        let stamp = std::fs::metadata(key).ok().and_then(|md| {
            let mtime = md
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            Some((md.len(), mtime))
        });
        if let Some((size, mtime)) = stamp {
            if let Some((s, m, sp)) = self.nar_uploads.lock().unwrap().get(key) {
                if *s == size && *m == mtime {
                    NAR_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(sp.clone());
                }
            }
        }
        NAR_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sp = self.add_to_store_nar(name, path)?;
        // Only cache what could be stamped: an unstattable key must stay a
        // miss forever rather than be remembered under a stamp of zero.
        if let Some((size, mtime)) = stamp {
            self.nar_uploads
                .lock()
                .unwrap()
                .insert(key.to_path_buf(), (size, mtime, sp.clone()));
        }
        Ok(sp)
    }

    /// Snapshot of the NAR stamp cache, for cross-run persistence
    /// (upstream #18's restart half): (key path, size, mtime_ns, store path).
    pub fn nar_stamps_snapshot(&self) -> Vec<(PathBuf, u64, u128, StorePath)> {
        self.nar_uploads
            .lock()
            .unwrap()
            .iter()
            .map(|(k, (s, m, sp))| (k.clone(), *s, *m, sp.clone()))
            .collect()
    }

    /// Seed the NAR stamp cache from a previous run's snapshot. Entries
    /// are still validated per hit by size+mtime against the live file;
    /// the caller filters for store paths that still exist on disk.
    pub fn seed_nar_stamps(&self, entries: Vec<(PathBuf, u64, u128, StorePath)>) {
        let mut map = self.nar_uploads.lock().unwrap();
        for (k, s, m, sp) in entries {
            map.entry(k).or_insert((s, m, sp));
        }
    }

    /// NAR `path` and upload it, STREAMING rather than buffering.
    ///
    /// This built the whole NAR into a `Vec<u8>` first, so every concurrent
    /// caller held an entire encoded tree in memory - and the caller is
    /// dependency discovery, which uploads a file per discovered include and
    /// sometimes a whole directory. The driver runs outside the build
    /// cgroup's ceiling, so nothing bounded the total; raising admission from
    /// 2 to 24 multiplied it and the workstation froze hard enough to need a
    /// power cycle.
    ///
    /// Admission control could not have saved it, and that is the part worth
    /// keeping: a memory taper refuses NEW work, while the memory here is
    /// committed by work already admitted and grows after admission. Rate
    /// limits do not bound a peak whose cost arrives later - the buffer had
    /// to stop existing.
    ///
    /// Peak is now the duplex capacity per upload instead of the tree size.
    /// The encoder runs on the blocking pool and the daemon drains the pipe,
    /// so a NAR larger than the buffer streams through rather than sitting in
    /// it. Safe against retries because there are none: `execute` is FnOnce
    /// and poisons its connection on error, and no caller retries this.
    pub fn add_to_store_nar(&self, name: &str, path: &Path) -> Result<StorePath> {
        let info = self
            .runtime
            .block_on(async {
                // Bounded pipe: the producer blocks when it is full rather than
                // growing, which is the whole point.
                let (writer, reader) = tokio::io::duplex(NAR_STREAM_BUF);
                let owned = path.to_path_buf();
                let producer = tokio::task::spawn_blocking(move || -> Result<()> {
                    let mut encoder =
                        nix_nar::Encoder::new(&owned).map_err(|e| Error::Nar(e.to_string()))?;
                    let mut bridge = tokio_util::io::SyncIoBridge::new(writer);
                    std::io::copy(&mut encoder, &mut bridge)?;
                    // Without the shutdown the reader never sees EOF and the
                    // daemon waits forever on a NAR that has already been sent.
                    std::io::Write::flush(&mut bridge)?;
                    bridge.shutdown()?;
                    Ok(())
                });
                let mut guard = self.pool.acquire().await?;
                let source = BufReader::new(reader);
                let out = if self.in_drv {
                    guard
                        .execute(|client| {
                            client.add_to_store_scanning(
                                name,
                                ContentAddressMethodAlgorithm::NixArchive(
                                    harmonia_utils_hash::Algorithm::SHA256,
                                ),
                                source,
                            )
                        })
                        .await
                } else {
                    // Suboptimal fallback, see add_to_store_text
                    let refs = Default::default();
                    guard
                        .execute(|client| {
                            client.add_ca_to_store(
                                name,
                                ContentAddressMethodAlgorithm::NixArchive(
                                    harmonia_utils_hash::Algorithm::SHA256,
                                ),
                                &refs,
                                false,
                                source,
                            )
                        })
                        .await
                };
                // Join AFTER the RPC: the daemon drains the pipe, so joining
                // first deadlocks on any NAR larger than the buffer. A producer
                // error must not be swallowed - an encoder that failed mid-tree
                // otherwise looks like a short but valid NAR.
                match producer.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(e),
                    Err(join) => return Err(Error::Nar(format!("nar encoder panicked: {join}"))),
                }
                // The early returns above fix this block's error type, so the
                // RPC's own error needs converting rather than being handed back
                // raw the way it was before the join existed.
                out.map_err(Error::from)
            })
            .map_err(|e| Error::AddToStore {
                name: name.to_string(),
                detail: format!("nar of {}", path.display()),
                source: Box::new(e),
            })?;
        Ok(info.path)
    }

    /// Build the given derived paths and return the store path each one
    /// resolves to, in input order. Only usable outside a `builder-rpc-v0`
    /// sandbox — the restricted allowlist does not include `BuildPaths`.
    /// Realise `paths`, skipping any this client has already realised.
    ///
    /// The cache is checked here rather than inside the request builder
    /// because everything below - the merged output specs, the printed-length
    /// split, the watchdog, the output pooling that maps a CA reply back to a
    /// request - is interdependent and correct. Wrapping it leaves all of that
    /// untouched and makes the saving a property of what is ASKED rather than
    /// of how the asking works.
    pub fn build_paths(
        &self,
        store_dir: &StoreDir,
        paths: &[SingleDerivedPath],
    ) -> Result<Vec<StorePath>> {
        // THE HIT PATH WAS O(PATHS ASKED) IN SYSCALLS, UNDER A GLOBAL LOCK.
        //
        // It computed a key string, then called `.exists()` on the cached
        // store path, for EVERY path of EVERY call, with `self.realised`
        // held across the whole loop. Round 90 measured 6,082,577 paths asked
        // against 38,378 sent - so the memo had removed 99.4% of the daemon
        // round trips and kept a stat and an allocation per ask, serialised.
        // At task 17,500 `realise` was 6905 s of a 13,050 s dyn phase while
        // the machine sat 88% idle with all 26 driver threads in state S:
        // blocked on this mutex, not computing.
        //
        // Two changes, and the existence check is KEPT rather than dropped -
        // its reason is still true, a collected path symlinked here fails
        // later and further from the cause.
        //
        //   Keys are built BEFORE the lock. An allocation is cheap; six
        //   million of them inside a contended mutex are not.
        //
        //   Existence is confirmed ONCE PER PATH instead of once per ask,
        //   and the stat runs with the lock RELEASED. What the check defends
        //   against is a collection during this run; a path re-asked for the
        //   thousandth time has already been confirmed, and re-confirming it
        //   is what made the cost scale with the input set rather than with
        //   the store. First confirmation still happens before any caller
        //   receives the path.
        let keys: Vec<String> = paths
            .iter()
            .map(|p| store_dir.display(p).to_string())
            .collect();

        let mut out: Vec<Option<StorePath>> = Vec::with_capacity(paths.len());
        let mut misses: Vec<SingleDerivedPath> = Vec::new();
        let mut miss_slots: Vec<usize> = Vec::new();
        // (slot, key, path) for hits whose existence has not been confirmed yet.
        let mut to_confirm: Vec<(usize, String, StorePath)> = Vec::new();
        {
            let cache = self.realised.lock().unwrap();
            for (i, p) in paths.iter().enumerate() {
                match cache.get(&keys[i]) {
                    Some((sp, true)) => out.push(Some(sp.clone())),
                    Some((sp, false)) => {
                        out.push(Some(sp.clone()));
                        to_confirm.push((i, keys[i].clone(), sp.clone()));
                    }
                    None => {
                        out.push(None);
                        miss_slots.push(i);
                        misses.push(p.clone());
                    }
                }
            }
        }

        // Lock released: the syscalls happen here.
        if !to_confirm.is_empty() {
            let mut confirmed: Vec<(String, StorePath)> = Vec::new();
            for (slot, key, sp) in to_confirm {
                if sp.to_absolute_path(store_dir).exists() {
                    confirmed.push((key, sp));
                } else {
                    // Collected under us. Demote to a miss exactly as before.
                    out[slot] = None;
                    miss_slots.push(slot);
                    misses.push(paths[slot].clone());
                    self.realised.lock().unwrap().remove(&key);
                }
            }
            if !confirmed.is_empty() {
                // MARK THE PATH THAT WAS STAT'D, NOT WHATEVER IS UNDER THE KEY
                // NOW. The lock is released across the stat, so between the
                // check and this write another thread can demote, remove and
                // re-realise the same key; a bare `get_mut(&key).1 = true`
                // would then stamp "existence confirmed" on a StorePath nobody
                // ever stat'd. Narrow, but the comment above promises first
                // confirmation precedes any caller receiving the path, and in
                // that interleaving it would not. Comparing the value keeps
                // the promise true. Reported by the specification session.
                let mut cache = self.realised.lock().unwrap();
                for (key, stated) in confirmed {
                    if let Some(e) = cache.get_mut(&key) {
                        if e.0 == stated {
                            e.1 = true;
                        }
                    }
                }
            }
            // A demotion appends out of order; the merge below pairs
            // `miss_slots[k]` with `built[k]`, so the two must agree.
            // Extracted rather than inlined because THIS is where a bug would
            // hide and `build_paths` cannot be unit-tested without a daemon.
            miss_slots = reorder_misses(&miss_slots, paths.len());
            misses = miss_slots.iter().map(|&i| paths[i].clone()).collect();
        }

        REALISE_ASKED.fetch_add(paths.len() as u64, std::sync::atomic::Ordering::Relaxed);
        REALISE_SENT.fetch_add(misses.len() as u64, std::sync::atomic::Ordering::Relaxed);

        // Every path already realised: no daemon round trip at all. This is
        // the common case on a resolve loop walking one component's TUs.
        if misses.is_empty() {
            return Ok(out.into_iter().map(|o| o.expect("all hits")).collect());
        }

        let built = self.build_paths_uncached(store_dir, &misses)?;
        {
            let mut cache = self.realised.lock().unwrap();
            for (&slot, sp) in miss_slots.iter().zip(built.iter()) {
                // true: freshly realised by the daemon, so it exists now and
                // needs no confirming stat on its first hit.
                cache.insert(keys[slot].clone(), (sp.clone(), true));
            }
        }
        Ok(merge_hits_and_misses(out, miss_slots, built))
    }

    fn build_paths_uncached(
        &self,
        store_dir: &StoreDir,
        paths: &[SingleDerivedPath],
    ) -> Result<Vec<StorePath>> {
        let _realise_timer = RealiseTimer {
            started: std::time::Instant::now(),
            paths: paths.len(),
        };
        // Dedupe per derivation with MERGED output specs: requesting the
        // same drv once per output (a codegen drv producing dozens of
        // headers appears dozens of times among a task's inputs) makes
        // the daemon merge them into one result whose DerivedPath key -
        // carrying the merged spec - matches none of the single-output
        // request keys, and every lookup then reports "no build result".
        // Key results by the drv/opaque path alone instead.
        let mut merged: Vec<DerivedPath> = Vec::new();
        let mut drv_index: HashMap<String, usize> = HashMap::new();
        for p in paths {
            match p {
                SingleDerivedPath::Opaque(path) => {
                    let key = path.to_string();
                    if let std::collections::hash_map::Entry::Vacant(e) = drv_index.entry(key) {
                        e.insert(merged.len());
                        merged.push(DerivedPath::Opaque(path.clone()));
                    }
                }
                SingleDerivedPath::Built { drv_path, output } => {
                    let key = format!("drv:{}", store_dir.display(drv_path.as_ref()));
                    match drv_index.get(&key) {
                        Some(&i) => {
                            if let DerivedPath::Built {
                                outputs: OutputSpec::Named(set),
                                ..
                            } = &mut merged[i]
                            {
                                set.insert(output.clone());
                            }
                        }
                        None => {
                            drv_index.insert(key, merged.len());
                            merged.push(DerivedPath::Built {
                                drv_path: drv_path.clone(),
                                outputs: OutputSpec::Named(
                                    std::iter::once(output.clone()).collect(),
                                ),
                            });
                        }
                    }
                }
            }
        }

        // The wire writer formats each DerivedPath through a bounded
        // display buffer (harmonia's display_buf_size, 8 KiB, no public
        // setter on DaemonClientBuilder): one Built entry whose merged
        // output set prints as `drv^o1,o2,...` past that bound fails the
        // whole request as `io error: an error occurred when formatting
        // an argument` - fmt::Error with the real cause erased. A task
        // inheriting hundreds of outputs of ONE producer drv (the
        // configure set) crosses it. Split any oversized entry into
        // several Built entries for the same drv, budgeted by the
        // PRINTED length; the daemon accepts duplicates and the reply
        // handling below pools realisations by output name anyway.
        const MAX_SPEC_CHARS: usize = 4096;
        let merged: Vec<DerivedPath> = merged
            .into_iter()
            .flat_map(|p| match p {
                DerivedPath::Built {
                    drv_path,
                    outputs: OutputSpec::Named(set),
                } if set.iter().map(|o| o.as_ref().len() + 1).sum::<usize>() > MAX_SPEC_CHARS => {
                    let mut chunks: Vec<DerivedPath> = Vec::new();
                    let mut cur: Vec<_> = Vec::new();
                    let mut cur_len = 0usize;
                    for o in set {
                        let l = o.as_ref().len() + 1;
                        if cur_len + l > MAX_SPEC_CHARS && !cur.is_empty() {
                            chunks.push(DerivedPath::Built {
                                drv_path: drv_path.clone(),
                                outputs: OutputSpec::Named(
                                    std::mem::take(&mut cur).into_iter().collect(),
                                ),
                            });
                            cur_len = 0;
                        }
                        cur_len += l;
                        cur.push(o);
                    }
                    if !cur.is_empty() {
                        chunks.push(DerivedPath::Built {
                            drv_path,
                            outputs: OutputSpec::Named(cur.into_iter().collect()),
                        });
                    }
                    chunks
                }
                other => vec![other],
            })
            .collect();

        // Watchdog against a daemon-side wedge, measured 2026-08-20 on the
        // qtwebengine graph: under ~20 concurrent build requests, daemon
        // children go dead-asleep mid-build - build locks held, zero CPU,
        // zero context switches over 20s, no kernel lock waiters, no
        // builder processes, nothing in the daemon log - while the client
        // side parks forever awaiting the reply. Root cause is on the
        // daemon side of the socket and unreachable from here; what IS
        // reachable is the connection: closing it kills the stuck daemon
        // child, which releases its locks (verified - every driver kill
        // unwedged the daemon). So: bound the wait, drop the wedged
        // connection (the guard is dirty mid-execute, so drop discards
        // rather than recirculates it), and retry on a fresh one. Builds
        // are idempotent daemon-side. The allowance escalates 4x per
        // attempt so a genuinely long single build (the terminal link)
        // that trips a false positive still converges: killed once at
        // 300s, it gets 1200s, then 4800s twice (attempts 0-3).
        // The retry path needs its own concurrency cap, measured round 81:
        // the wedge is load-triggered (~20 concurrent build requests), and
        // a mass timeout retries all ~20 requests SIMULTANEOUSLY - the
        // recovery recreates the trigger, the fresh daemon children wedge
        // on contact (19 attempt-2 timeouts in a row, children born
        // 08:03:27 dead-asleep by 08:04), and the loop converges only on
        // its own attempt budget. First-attempt traffic arrives naturally
        // staggered and stays uncapped; retries queue through this gate so
        // recovering requests trickle back below the wedge threshold.
        static RETRY_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        // Gauge of build requests currently awaiting a daemon reply,
        // counted AFTER the pool grants a connection - it reads daemon-side
        // concurrency (bounded by pool size) and is blind to pool-queued
        // requests, which is the concurrency the wedge cares about. The
        // peak logs once per new high-water mark: the wedge trigger
        // ("~20 concurrent") was inference for three days because the
        // gauge only ever printed after the fact.
        static IN_FLIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static PEAK_IN_FLIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        fn now_s() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        }
        use std::sync::atomic::Ordering::Relaxed;
        // WHAT THE WAIT IS FOR. A stall message naming only a duration and a
        // count says the same thing every time it fires, so eleven timeouts
        // across four packages in one round were indistinguishable from each
        // other and from a hang with no subject. Named by the first path
        // because a request carries one derivation's outputs in the common
        // case; the count covers the rest without printing a store path per
        // output at 16,000 tasks.
        let waited_on = {
            let first = paths
                .first()
                .map(|p| match p {
                    SingleDerivedPath::Opaque(sp) => store_dir.display(sp).to_string(),
                    SingleDerivedPath::Built { drv_path, output } => {
                        format!("{}^{output}", store_dir.display(drv_path.as_ref()))
                    }
                })
                .unwrap_or_else(|| "<nothing>".to_string());
            if paths.len() > 1 {
                format!("{first} (+{} more)", paths.len() - 1)
            } else {
                first
            }
        };
        let results = self.runtime.block_on(async {
            // Two counters, deliberately not one: with a shared budget,
            // COUNTS THIS CALL, NOT THIS RUN, and the message now says so.
            // `stall_attempts` is a local of the per-call retry loop, so round
            // 90's three watchdog timeouts each printed "stalls 1" and the run
            // total of 3 was recoverable only by counting log lines - a number
            // that reads as a total while being a per-call figure. Fixed in the
            // wording rather than with a second counter: the per-call number is
            // the one the retry logic acts on, and a run total that nothing
            // reads is a counter to keep correct for no consumer. Reported by
            // the specification session.
            // three stalls plus one connect failure would surface as a
            // plain connect error (the wedge evidence discarded), and
            // three connect failures would consume the allowance
            // escalation a genuinely long build depends on.
            let mut stall_attempts: u32 = 0;
            let mut connect_failures: u32 = 0;
            loop {
                let retrying = stall_attempts + connect_failures > 0;
                let allowance_s = stall_allowance_s(stall_attempts);
                let mut _gate = if retrying {
                    Some(
                        RETRY_GATE
                            .acquire()
                            .await
                            .expect("retry gate is never closed"),
                    )
                } else {
                    None
                };
                // A connect can also fail transiently: right after a mass
                // wedge recovery the daemon is busy reaping ~20 killed
                // children and fresh accepts time out (measured round 80:
                // one "timeout: connecting to daemon" ended the round a
                // minute after the recovery worked).
                let mut guard = match self.pool.acquire().await {
                    Ok(g) => g,
                    Err(e) => {
                        connect_failures += 1;
                        if connect_failures > CONNECT_RETRIES {
                            eprintln!(
                                "nix-ninja: [{}] WATCHDOG giving-up: connect failed \
                                 {connect_failures} times over {}s ({stall_attempts} stalls \
                                 before it)",
                                now_s(),
                                CONNECT_BACKOFF_S.iter().sum::<u64>(),
                            );
                            break Err(Error::from(e));
                        }
                        // Backoff rather than a flat 10s: the failure this
                        // covers is the daemon being DOWN, not busy, and a
                        // restart is not over in 30 seconds. Measured
                        // 2026-08-20: a restart to install a config change
                        // killed a round that was 45 minutes in and had
                        // 15,000 tasks resolved, at "connect failures 1".
                        // The whole ladder is ~3 minutes, which rides a
                        // restart and still surrenders long before a
                        // genuinely dead daemon wastes a night.
                        let wait = CONNECT_BACKOFF_S
                            [(connect_failures as usize - 1).min(CONNECT_BACKOFF_S.len() - 1)];
                        eprintln!(
                            "nix-ninja: [{}] WATCHDOG connect-fail ({e}); retrying in {wait}s \
                             (connect failures {connect_failures}/{CONNECT_RETRIES}, \
                             stalls this call {stall_attempts})",
                            now_s(),
                        );
                        // Sleep OUTSIDE the gate permit: holding one of the
                        // two recovery lanes through the backoff starves the
                        // other retryers for no reason, and the wait is now
                        // up to 60s rather than the flat 10s this comment
                        // was written against.
                        _gate = None;
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                };
                let in_now = IN_FLIGHT.fetch_add(1, Relaxed) + 1;
                if in_now > PEAK_IN_FLIGHT.fetch_max(in_now, Relaxed) {
                    eprintln!("nix-ninja: [{}] WATCHDOG peak in-flight {in_now}", now_s());
                }
                let started_wall = now_s();
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(allowance_s),
                    guard.execute(|client| {
                        client.build_paths_with_results(&merged, BuildMode::Normal)
                    }),
                )
                .await;
                let others = IN_FLIGHT.fetch_sub(1, Relaxed).saturating_sub(1);
                match outcome {
                    Ok(Ok(res)) => {
                        // Recovery success was silent before this line, so
                        // the only readable verdict on a retry policy was
                        // the absence of the next timeout - a weak signal.
                        if retrying {
                            eprintln!(
                                "nix-ninja: [{}] WATCHDOG recovered after {stall_attempts} \
                                 stall(s) and {connect_failures} connect failure(s), \
                                 {}s this attempt ({others} others in flight)",
                                now_s(),
                                now_s().saturating_sub(started_wall),
                            );
                        }
                        break Ok(res);
                    }
                    Ok(Err(e)) => {
                        // A mid-execute CONNECTION error is the wedge family
                        // wearing an error instead of a hang, and recovery
                        // itself inflicts it on siblings: dropping a wedged
                        // connection kills a daemon child, and the reaping
                        // daemon resets its neighbours. Retry those on a
                        // fresh connection exactly like a timeout. A daemon-
                        // REPORTED error (Remote) is a real verdict about
                        // the request and stays terminal.
                        let connection_level = matches!(
                            e.kind(),
                            DaemonErrorKind::IO(_)
                                | DaemonErrorKind::WrongMagic(_)
                                | DaemonErrorKind::UnsupportedVersion(_)
                                | DaemonErrorKind::NoSinkForLoggerWrite
                                | DaemonErrorKind::NoSourceForLoggerRead
                        );
                        // ONE daemon-reported error is transient and named:
                        // the daemon's own cgroup teardown races its other
                        // builds, so a worker opens
                        // nix-build-uid-N/cgroup.{kill,procs} after a
                        // sibling finished and removed it - ENOENT on a
                        // path the daemon itself manages (libxcrypt under
                        // make -j, 2026-08-23; an upstream nix bug, not a
                        // verdict about this request). Retry it like a
                        // connection error, bounded by the same counter and
                        // logged, so a genuine failure still surfaces.
                        if !connection_level && !transient_daemon_error(&e.to_string()) {
                            break Err(Error::from(e));
                        }
                        drop(guard);
                        stall_attempts += 1;
                        if stall_attempts > 3 {
                            break Err(Error::DaemonStalled {
                                attempts: stall_attempts,
                                connect_failures,
                                last_allowance_s: allowance_s,
                            });
                        }
                        eprintln!(
                            "nix-ninja: [{}] WATCHDOG conn-error ({e}) after {}s; dropped \
                             the connection and retrying (stalls this call {stall_attempts})",
                            now_s(),
                            now_s().saturating_sub(started_wall),
                        );
                    }
                    Err(_elapsed) => {
                        drop(guard);
                        stall_attempts += 1;
                        if stall_attempts > 3 {
                            break Err(Error::DaemonStalled {
                                attempts: stall_attempts,
                                connect_failures,
                                last_allowance_s: allowance_s,
                            });
                        }
                        eprintln!(
                            "nix-ninja: [{}] WATCHDOG timeout on {waited_on}: no \
                             daemon reply in {allowance_s}s, waited since \
                             [{started_wall}] ({others} others in flight); dropped \
                             the wedged connection (frees the stuck daemon child's \
                             locks) and retrying (stalls this call {stall_attempts})",
                            now_s(),
                        );
                    }
                }
            }
        })?;

        // The daemon replies keyed by the RESOLVED drv path while the
        // request carries the unresolved one (CA derivations), so no
        // path-keyed map can match in general. Pool every returned
        // realisation by OUTPUT NAME - names derive from file paths and
        // are unique within one request - and keep the path-keyed map
        // as a first-chance failure reporter where paths do agree.
        let mut by_key: HashMap<String, _> = HashMap::new();
        let mut out_pool: HashMap<OutputName, StorePath> = HashMap::new();
        // ALL failures, not the last one: with two failed derivations a
        // single slot names one of them nondeterministically, and the
        // missing-result classification below needs to know whether ANY
        // failure occurred, not which arrived last.
        //
        // EACH ONE CARRIES THE PATH THAT FAILED, and dropping it is what ends
        // a diagnosis. A realise is a BATCH, so when a task dies of
        // "1 dependency failed" the dependency's own result is usually in this
        // same reply; keeping only the message reported the target that
        // depended on the failure and never the derivation that produced it,
        // leaving nothing to run `nix log` against.
        let mut failures: Vec<String> = Vec::new();
        for r in results {
            if let Some(success) = r.result.success() {
                for (name, realisation) in &success.built_outputs {
                    out_pool.insert(name.clone(), realisation.out_path.clone());
                }
            } else if let BuildResultInner::Failure(f) = &r.result.inner {
                let who = match &r.path {
                    DerivedPath::Opaque(path) => store_dir.display(path).to_string(),
                    DerivedPath::Built { drv_path, .. } => {
                        store_dir.display(drv_path.as_ref()).to_string()
                    }
                };
                failures.push(format!("{who}: {}", String::from_utf8_lossy(&f.error_msg)));
            }
            let key = match &r.path {
                DerivedPath::Opaque(path) => path.to_string(),
                DerivedPath::Built { drv_path, .. } => {
                    format!("drv:{}", store_dir.display(drv_path.as_ref()))
                }
            };
            by_key.insert(key, r.result);
        }
        let others = |own: &str| co_failure_summary(&failures, own);

        paths
            .iter()
            .map(|single| {
                let display = store_dir.display(single).to_string();
                let key = match single {
                    SingleDerivedPath::Opaque(path) => path.to_string(),
                    SingleDerivedPath::Built { drv_path, .. } => {
                        format!("drv:{}", store_dir.display(drv_path.as_ref()))
                    }
                };
                match single {
                    SingleDerivedPath::Opaque(path) => {
                        // An opaque path either matched by key or was a
                        // no-op; failures still surface below.
                        match by_key.get(&key) {
                            Some(result) if result.success().is_none() => {
                                let own = match &result.inner {
                                    BuildResultInner::Failure(f) => {
                                        String::from_utf8_lossy(&f.error_msg).into_owned()
                                    }
                                    _ => String::new(),
                                };
                                let extra = others(&display);
                                Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: format!("{own}{extra}"),
                                })
                            }
                            _ => Ok(path.clone()),
                        }
                    }
                    SingleDerivedPath::Built { output, .. } => {
                        if let Some(result) = by_key.get(&key) {
                            if result.success().is_none() {
                                let own = match &result.inner {
                                    BuildResultInner::Failure(f) => {
                                        String::from_utf8_lossy(&f.error_msg).into_owned()
                                    }
                                    _ => String::new(),
                                };
                                let extra = others(&display);
                                return Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: format!("{own}{extra}"),
                                });
                            }
                        }
                        out_pool.get(output).cloned().ok_or_else(|| {
                            // A CA derivation's reply is keyed by the RESOLVED
                            // path, so a genuine build failure misses both maps
                            // and used to surface as MissingBuildResult - the
                            // right facts under the wrong error. When any
                            // failure exists, report it as the failure it is.
                            if failures.is_empty() {
                                Error::MissingBuildResult(format!(
                                    "{display} (no realisation named '{output}' in {} pooled outputs)",
                                    out_pool.len(),
                                ))
                            } else {
                                Error::BuildFailed {
                                    path: display.clone(),
                                    error_msg: failures.join("; "),
                                }
                            }
                        })
                    }
                }
            })
            .collect()
    }

    /// Declare `path` as the named output of the currently-running
    /// derivation. The path's name must equal
    /// `outputPathName(callingDrv.name, name)`.
    pub fn submit_output(&self, path: &SingleDerivedPath, name: &OutputName) -> Result<()> {
        self.runtime.block_on(async {
            let mut guard = self.pool.acquire().await?;
            guard
                .execute(|client| client.submit_output(path, name))
                .await
        })?;
        Ok(())
    }
}

/// `$NIX_REMOTE` is typically `unix:///abs/path/to/socket` or the legacy
/// alias `daemon`/`auto` that means "default socket". Anything else (e.g.
/// `https://...`, `s3://...`) is unsupported here.
fn parse_unix_remote(remote: &str) -> Result<PathBuf> {
    if matches!(remote, "daemon" | "auto" | "") {
        return Ok(PathBuf::from(DEFAULT_DAEMON_SOCKET));
    }
    if let Some(stripped) = remote.strip_prefix("unix://") {
        return Ok(PathBuf::from(stripped));
    }
    if remote.starts_with('/') {
        return Ok(PathBuf::from(remote));
    }
    Err(Error::UnsupportedRemote(remote.to_string()))
}

/// Uploads served from the memo, and uploads that reached the daemon. Printed
/// by the driver's tick so the saving is measurable rather than asserted.
static NAR_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NAR_UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(hits, uploads)` for the NAR memo.
pub fn nar_upload_stats() -> (u64, u64) {
    (
        NAR_HITS.load(std::sync::atomic::Ordering::Relaxed),
        NAR_UPLOADS.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Bytes in flight per NAR upload.
///
/// Replaced an unbounded `Vec<u8>` holding the whole encoded tree. This is a
/// pipe size rather than a tuning knob: big enough that a small header moves
/// in one go, small enough that a directory upload cannot be a memory event.
/// Peak per upload is this, not the tree.
const NAR_STREAM_BUF: usize = 256 * 1024;
// The buffer is a pipe size, not a tuning knob, but a zero or absurd value
// silently changes it into one - duplex(0) blocks forever. Compile-time for
// the same reason as SLOW_REALISE_MS above: this is a fact about the
// constant, and a #[test] asserting it can never fail.
const _: () = assert!(
    NAR_STREAM_BUF >= 64 * 1024,
    "too small to move a header in one go"
);
const _: () = assert!(
    NAR_STREAM_BUF <= 4 * 1024 * 1024,
    "large enough to be a memory event again"
);

#[cfg(test)]
mod connect_backoff_tests {
    use super::{CONNECT_BACKOFF_S, CONNECT_RETRIES};

    /// The retry count is DERIVED from the ladder. If someone raises the
    /// count alone, the index clamp silently repeats the last wait instead
    /// of erroring, so the coupling is the thing worth asserting.
    #[test]
    fn retries_match_the_backoff_ladder() {
        assert_eq!(CONNECT_RETRIES as usize, CONNECT_BACKOFF_S.len());
    }

    /// The window has to outlast a daemon restart. Round 84 died at
    /// "connect failures 1" under a flat 10s x 3, which is 30s; a restart
    /// takes longer. Anything under two minutes reopens that hole.
    #[test]
    fn total_window_rides_a_daemon_restart() {
        let total: u64 = CONNECT_BACKOFF_S.iter().sum();
        assert!(
            total >= 120,
            "connect window {total}s is too short for a restart"
        );
        // and bounded, so a genuinely dead daemon does not burn a night
        assert!(
            total <= 600,
            "connect window {total}s waits too long on a dead daemon"
        );
    }

    /// Monotonic: each wait at least as long as the one before it. A ladder
    /// that dips would spend its attempts fastest exactly when the outage
    /// has proven it is not brief.
    #[test]
    fn backoff_never_shortens() {
        for w in CONNECT_BACKOFF_S.windows(2) {
            assert!(w[1] >= w[0], "backoff dips: {:?}", CONNECT_BACKOFF_S);
        }
    }
}

#[cfg(test)]
mod realise_timer_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The guard must record on EVERY exit, which is the whole reason it is a
    /// Drop impl and not a timed block. An early return that skips the
    /// accounting would understate exactly the slow call worth catching.
    #[test]
    fn dropping_the_guard_records_a_call() {
        let before = BUILD_PATHS_CALLS.load(Ordering::Relaxed);
        {
            let _t = RealiseTimer {
                started: std::time::Instant::now(),
                paths: 7,
            };
        }
        assert_eq!(BUILD_PATHS_CALLS.load(Ordering::Relaxed), before + 1);
    }

    /// The threshold is quoted in the comment above it and in docs/errata.md;
    /// pin it so a silent edit cannot make the loud case quiet again.
    #[test]
    fn slow_threshold_stays_loud_enough_to_catch_a_stalled_round() {
        assert_eq!(SLOW_REALISE_MS, 30_000);
    }
}

#[cfg(test)]
mod realise_memo_tests {
    use super::{merge_hits_and_misses, reorder_misses};

    /// The function was extracted because THIS is where a bug would hide, and
    /// then nothing tested it. A demotion pushes a slot AFTER the original
    /// misses, so the input is not ascending; the caller rebuilds the request
    /// vector from the result and `merge_hits_and_misses` pairs
    /// `miss_slots[k]` with `built[k]`, so a non-ascending return hands a real
    /// store path to the WRONG input and fails three layers away in a compile.
    #[test]
    fn a_demoted_slot_is_sorted_back_into_place() {
        // Slots 1 and 4 missed outright; slot 0 was a hit demoted by the stat,
        // so it arrives last. Ascending order is what the merge requires.
        assert_eq!(reorder_misses(&[1, 4, 0], 5), vec![0, 1, 4]);
    }

    /// A slot outside the caller's path count is a bug here, not upstream, and
    /// must be dropped rather than panic the driver by indexing out of bounds
    /// when the caller maps the result back over `paths`.
    #[test]
    fn an_out_of_range_slot_is_dropped_not_indexed() {
        assert_eq!(reorder_misses(&[2, 9, 0], 3), vec![0, 2]);
        assert!(reorder_misses(&[7], 3).is_empty());
    }

    /// Duplicates cannot arise today - a demoted slot was a hit, so it was
    /// never in `miss_slots` - but the dedup is load-bearing if that ever
    /// changes: one duplicate slot asks the daemon twice and shifts every
    /// later pairing by one.
    #[test]
    fn a_duplicate_slot_would_shift_every_later_pairing() {
        assert_eq!(reorder_misses(&[3, 1, 3], 5), vec![1, 3]);
    }

    /// The no-demotion path: already ascending, returned unchanged. If this
    /// ever reorders, every ordinary call is wrong, not just the rare one.
    #[test]
    fn the_ordinary_case_is_left_alone() {
        assert_eq!(reorder_misses(&[0, 2, 3], 4), vec![0, 2, 3]);
        assert!(reorder_misses(&[], 4).is_empty());
    }

    /// Hits and misses interleaved is the case that orders wrongly if the
    /// merge indexes by position in `built` rather than by recorded slot.
    #[test]
    fn interleaved_hits_and_misses_keep_request_order() {
        let slots = vec![Some("hit0"), None, Some("hit2"), None, None];
        let merged = merge_hits_and_misses(slots, vec![1, 3, 4], vec!["m1", "m3", "m4"]);
        assert_eq!(merged, vec!["hit0", "m1", "hit2", "m3", "m4"]);
    }

    /// An all-hit request never reaches the daemon; an all-miss one is the
    /// pre-memo behaviour and must be unchanged.
    #[test]
    fn the_two_extremes_are_both_identity() {
        assert_eq!(
            merge_hits_and_misses(vec![Some("a"), Some("b")], vec![], vec![]),
            vec!["a", "b"]
        );
        assert_eq!(
            merge_hits_and_misses(vec![None, None], vec![0, 1], vec!["x", "y"]),
            vec!["x", "y"]
        );
    }

    /// A short reply must abort rather than leave a slot holding a
    /// neighbour's path, which is the corruption this function exists to
    /// make impossible.
    #[test]
    #[should_panic(expected = "realise returned")]
    fn a_short_reply_is_refused_not_absorbed() {
        merge_hits_and_misses(vec![None, None], vec![0, 1], vec!["only-one"]);
    }
}

#[cfg(test)]
mod uploaded_drv_retention_tests {
    /// Retention is gated on `in_drv`, and the reason is asymmetric: inside a
    /// derivation the uploaded .drv is not materialized and the retained copy
    /// is the only one, so dropping it there loses data. Outside, the file is
    /// on disk. A future edit that flips this to an unconditional skip would
    /// break the sandbox path silently - the failure is a missing drv late in
    /// a build, nowhere near the cache.
    ///
    /// Asserted on the source because the branch needs a live daemon to
    /// exercise and the INVARIANT is what must not drift.
    ///
    /// MATCHED LINE-WISE, NOT ON EXACT INDENTATION. The first version searched
    /// for the map name followed by a newline and exactly sixteen spaces, so
    /// any reformat made the search return None and the test panicked with
    /// "the retaining insert must still exist" - naming a deletion that had
    /// not happened. Loud rather than silent, but a diagnostic that asserts
    /// the wrong cause sends the next reader after the wrong thing, which is
    /// worse than one that asserts nothing. Found by the specification
    /// session.
    #[test]
    fn retention_is_conditional_on_being_inside_a_derivation() {
        let src: Vec<&str> = include_str!("lib.rs").lines().collect();
        let insert = find_insert(&src).expect("the retaining insert must still exist");
        let guard = find_guard(&src, insert).expect("the insert must sit under an in_drv guard");
        assert!(
            insert - guard < 4,
            "the in_drv guard drifted away from the insert it protects"
        );
    }

    /// The check above must fail when the guard is gone, or it asserts
    /// nothing - and it must NOT fail merely because the code was reformatted,
    /// which is how the previous version broke. Both directions, on the two
    /// matchers the live check uses, so the control cannot drift from it.
    #[test]
    fn the_matchers_survive_a_reformat_and_fail_without_the_guard() {
        let reformatted: Vec<&str> = vec![
            "        if self.in_drv {",
            "                        self.uploaded_drvs.lock().unwrap()",
            "            .insert(info.path.clone(), bytes);",
            "        }",
        ];
        let insert = find_insert(&reformatted).expect("indentation must not matter");
        assert_eq!(find_guard(&reformatted, insert), Some(0));

        // The map is named and the retention is gone: keying on the name
        // alone would pass here, which is the whole point of the window.
        let read_only: Vec<&str> =
            vec!["        if let Some(b) = self.uploaded_drvs.lock().unwrap().get(p) {"];
        assert_eq!(find_insert(&read_only), None, "a read is not a retention");

        let unguarded: Vec<&str> = vec![
            "        self.uploaded_drvs",
            "            .lock()",
            "            .unwrap()",
            "            .insert(info.path.clone(), bytes);",
        ];
        let insert = find_insert(&unguarded).expect("the insert is still there");
        assert_eq!(find_guard(&unguarded, insert), None, "no guard to find");
    }

    /// The line retaining the uploaded .drv, found by CONTENT rather than by
    /// indentation. The window is what separates it from the read a few lines
    /// above: both name the map, only one of them inserts.
    ///
    /// Needles are built at runtime so they never appear as literals in the
    /// file this searches - which is this file. A pattern that matches its own
    /// source finds itself and reports a pass.
    fn find_insert(lines: &[&str]) -> Option<usize> {
        let map = concat!("self.", "uploaded_drvs");
        (0..lines.len()).find(|&i| {
            lines[i].contains(map)
                && lines[i..(i + 4).min(lines.len())]
                    .iter()
                    .any(|l| l.contains(".insert("))
        })
    }

    /// The nearest `in_drv` guard above a given insert.
    fn find_guard(lines: &[&str], insert: usize) -> Option<usize> {
        let needle = concat!("if self.", "in_drv {");
        lines[..insert].iter().rposition(|l| l.contains(needle))
    }
}

#[cfg(test)]
mod nar_streaming_tests {
    use super::NAR_STREAM_BUF;
    use tokio::io::AsyncReadExt;

    /// A NAR larger than the pipe must stream through.
    ///
    /// This is the shape that used to be a `Vec<u8>`, and the two ways to get
    /// it wrong both hang rather than fail: forget the shutdown and the
    /// reader never sees EOF, or join the producer before draining and the
    /// producer blocks on a full pipe forever. Both would have looked like
    /// the daemon wedge this driver already has a watchdog for, which is
    /// exactly the wrong place to go looking.
    #[test]
    fn a_payload_larger_than_the_buffer_streams_without_deadlock() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Three buffers' worth: forces the producer to block and be drained
        // more than once, which one buffer would not.
        let payload = vec![0xABu8; NAR_STREAM_BUF * 3 + 7];
        let expected = payload.len();

        let got = rt.block_on(async move {
            let (writer, mut reader) = tokio::io::duplex(NAR_STREAM_BUF);
            let producer = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                let mut src = std::io::Cursor::new(payload);
                let mut bridge = tokio_util::io::SyncIoBridge::new(writer);
                std::io::copy(&mut src, &mut bridge)?;
                std::io::Write::flush(&mut bridge)?;
                bridge.shutdown()?;
                Ok(())
            });
            let mut sink = Vec::new();
            // Drain FIRST, join second - the order the real call site uses.
            reader.read_to_end(&mut sink).await.unwrap();
            producer.await.unwrap().unwrap();
            sink.len()
        });

        assert_eq!(got, expected, "streamed byte count must match the source");
    }
}

#[cfg(test)]
mod nar_upload_memo_tests {
    use std::io::Write;

    /// A build-directory file is MUTABLE, which is what separates this memo
    /// from the realise one. The dangerous direction is a generated header
    /// rewritten mid-round and served from the cache afterwards: the task
    /// would take a store path for content that no longer exists anywhere,
    /// and the compile fails far from here. Size-and-mtime is the guard, and
    /// this asserts it actually discriminates rather than just being present.
    #[test]
    fn a_rewritten_file_changes_its_stamp() {
        let dir = std::env::temp_dir().join(format!("nn-narmemo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("generated.h");

        let stamp = |p: &std::path::Path| {
            let md = std::fs::metadata(p).unwrap();
            (
                md.len(),
                md.modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            )
        };

        std::fs::File::create(&f)
            .unwrap()
            .write_all(b"#define A 1\n")
            .unwrap();
        let before = stamp(&f);

        // Same length, different content: the size half alone would call this
        // unchanged, which is exactly the case a regenerated header hits.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::File::create(&f)
            .unwrap()
            .write_all(b"#define A 2\n")
            .unwrap();
        let after = stamp(&f);

        assert_eq!(before.0, after.0, "test is only meaningful at equal size");
        assert_ne!(
            before, after,
            "equal-size rewrite must still change the stamp"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unstattable key must never be memoized: remembering it under a
    /// zero stamp would serve one file's store path for another's.
    #[test]
    fn an_unstattable_key_is_not_cacheable() {
        let missing = std::path::Path::new("/nonexistent/nn-memo-probe");
        assert!(std::fs::metadata(missing).is_err());
    }
}

/// Daemon-REPORTED errors that are races in the daemon rather than
/// verdicts about the request, retried bounded exactly like a
/// connection error so a genuine failure still surfaces.
///
/// - cgroup teardown race: the daemon's own cgroup teardown races its
///   other builds, so a worker opens nix-build-uid-N/cgroup.{kill,procs}
///   after a sibling finished and removed it - ENOENT on a path the
///   daemon itself manages (libxcrypt under make -j, 2026-08-23).
/// - user-lock euid race: "the Nix user should not be a member of
///   'nixbld'" from SimpleUserLock::acquire's sanity check
///   (lock->uid == getuid() || geteuid()) inside the multithreaded
///   daemon worker, whose euid another thread can hold at a build
///   user's uid at the instant the recursive daemon thread acquires a
///   lock. Measured 2026-08-23: one refusal among concurrent successes
///   in the same gnutar sandbox, unreproducible serially on the same
///   derivation; no nixbld member shares a uid with the daemon, so a
///   clean acquire cannot throw. An upstream nix bug, not a verdict.
///   Polarity: a GENUINE misconfiguration (a root-uid group member)
///   fails every retry and still surfaces after the bounded ladder.
fn transient_daemon_error(msg: &str) -> bool {
    // The cgroup race has TWO signatures, one per side of the same
    // daemon defect: a worker tearing down a per-uid cgroup another
    // worker still holds reads ENOENT, and a worker CREATING one that a
    // dying worker has not yet removed reads EEXIST ("creating cgroup
    // .../nix-build-uid-N: File exists" - fftw, 2026-08-23). Both are
    // scoped to the cgroup path so an unrelated File exists (a real
    // output collision) stays a verdict.
    (msg.contains("/sys/fs/cgroup/")
        && (msg.contains("No such file") || msg.contains("File exists")))
        || msg.contains("should not be a member of")
}

#[cfg(test)]
mod transient_error_tests {
    use super::transient_daemon_error;

    #[test]
    fn the_two_named_races_are_transient_and_verdicts_are_not() {
        assert!(transient_daemon_error(
            "opening file '/sys/fs/cgroup/nixbuild/nix-daemon/nix-build-uid-952/cgroup.kill': No such file or directory"
        ));
        assert!(transient_daemon_error(
            "BuildPathsWithResults: remote error: the Nix user should not be a member of 'nixbld'"
        ));
        // The create side of the same cgroup race (fftw, 2026-08-23).
        assert!(transient_daemon_error(
            "creating cgroup \"/sys/fs/cgroup/nixbuild/nix-daemon/nix-build-uid-939\": File exists"
        ));
        // Negative controls: real verdicts must stay terminal.
        assert!(!transient_daemon_error("builder failed with exit code 2"));
        assert!(!transient_daemon_error(
            "cannot build '/nix/store/x.drv' in recursive Nix because path is unknown"
        ));
        assert!(!transient_daemon_error(
            "missing system features: builder-rpc-v0"
        ));
        // A File exists OUTSIDE the cgroup path is a real collision.
        assert!(!transient_daemon_error(
            "copying '/build/out' to '/nix/store/x': File exists"
        ));
    }
}
