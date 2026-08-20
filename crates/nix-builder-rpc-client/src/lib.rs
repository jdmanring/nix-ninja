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
use harmonia_store_derivation::derivation::Derivation;
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
        assert!(line.contains("DaemonStalled"), "monitor grep would miss: {line}");
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

        Self::connect_unix_sized(&path, in_drv, pool_max.unwrap_or(PoolConfig::default().max_size))
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
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
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
        })
    }

    pub fn clone_drv(&self, store_path: &StorePath) -> Option<Vec<u8>> {
        self.uploaded_drvs.lock().unwrap().get(store_path).cloned()
    }

    // Serialise a derivation and add it to the store.
    // Should be preferred to `add_to_store_text` for derivations,
    pub fn add_drv_to_store(&self, store_dir: &StoreDir, drv: &Derivation) -> Result<StorePath> {
        let bytes = aterm::print_derivation_aterm(store_dir, drv);
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

        self.uploaded_drvs
            .lock()
            .unwrap()
            .insert(info.path.clone(), bytes);
        Ok(info.path)
    }

    /// Add bytes as a text-CA store object.
    /// Use for small files, but never for derivations.
    /// Derivations have different reference scanning logic, implemented in the
    /// `add_drv_to_store` function
    pub fn add_to_store_text(&self, name: &str, bytes: &[u8]) -> Result<StorePath> {
        let info = self.runtime.block_on(async {
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
        })?;
        Ok(info.path)
    }

    /// NAR a filesystem path then upload it as a recursive-CA (NAR-hashed)
    /// store object.
    pub fn add_to_store_nar(&self, name: &str, path: &Path) -> Result<StorePath> {
        let nar_bytes = encode_nar(path)?;
        let info = self.runtime.block_on(async {
            let mut guard = self.pool.acquire().await?;
            let source = BufReader::new(Cursor::new(nar_bytes));
            if self.in_drv {
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
            }
        })?;
        Ok(info.path)
    }

    /// Build the given derived paths and return the store path each one
    /// resolves to, in input order. Only usable outside a `builder-rpc-v0`
    /// sandbox — the restricted allowlist does not include `BuildPaths`.
    pub fn build_paths(
        &self,
        store_dir: &StoreDir,
        paths: &[SingleDerivedPath],
    ) -> Result<Vec<StorePath>> {
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
                    if !drv_index.contains_key(&key) {
                        drv_index.insert(key, merged.len());
                        merged.push(DerivedPath::Opaque(path.clone()));
                    }
                }
                SingleDerivedPath::Built { drv_path, output } => {
                    let key = format!("drv:{}", store_dir.display(drv_path.as_ref()));
                    match drv_index.get(&key) {
                        Some(&i) => {
                            if let DerivedPath::Built { outputs, .. } = &mut merged[i] {
                                if let OutputSpec::Named(set) = outputs {
                                    set.insert(output.clone());
                                }
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
                DerivedPath::Built { drv_path, outputs: OutputSpec::Named(set) }
                    if set.iter().map(|o| o.as_ref().len() + 1).sum::<usize>()
                        > MAX_SPEC_CHARS =>
                {
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
        let results = self.runtime.block_on(async {
            // Two counters, deliberately not one: with a shared budget,
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
                    Some(RETRY_GATE.acquire().await.expect("retry gate is never closed"))
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
                             stalls {stall_attempts})",
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
                        if !connection_level {
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
                             the connection and retrying (stalls {stall_attempts})",
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
                            "nix-ninja: [{}] WATCHDOG timeout: no daemon reply in \
                             {allowance_s}s, waited since [{started_wall}] ({others} \
                             others in flight); dropped the wedged connection (frees \
                             the stuck daemon child's locks) and retrying \
                             (stalls {stall_attempts})",
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
        let mut failures: Vec<String> = Vec::new();
        for r in results {
            if let Some(success) = r.result.success() {
                for (name, realisation) in &success.built_outputs {
                    out_pool.insert(name.clone(), realisation.out_path.clone());
                }
            } else if let BuildResultInner::Failure(f) = &r.result.inner {
                failures.push(String::from_utf8_lossy(&f.error_msg).into_owned());
            }
            let key = match &r.path {
                DerivedPath::Opaque(path) => path.to_string(),
                DerivedPath::Built { drv_path, .. } => {
                    format!("drv:{}", store_dir.display(drv_path.as_ref()))
                }
            };
            by_key.insert(key, r.result);
        }
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
                                return Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: match &result.inner {
                                        BuildResultInner::Failure(f) => {
                                            String::from_utf8_lossy(&f.error_msg).into_owned()
                                        }
                                        _ => String::new(),
                                    },
                                }
                                .into());
                            }
                            _ => Ok(path.clone()),
                        }
                    }
                    SingleDerivedPath::Built { output, .. } => {
                        if let Some(result) = by_key.get(&key) {
                            if result.success().is_none() {
                                return Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: match &result.inner {
                                        BuildResultInner::Failure(f) => {
                                            String::from_utf8_lossy(&f.error_msg).into_owned()
                                        }
                                        _ => String::new(),
                                    },
                                }
                                .into());
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

fn encode_nar(path: &Path) -> Result<Vec<u8>> {
    let mut encoder = nix_nar::Encoder::new(path).map_err(|e| Error::Nar(e.to_string()))?;
    let mut buf = Vec::new();
    std::io::copy(&mut encoder, &mut buf)?;
    Ok(buf)
}

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
        assert!(total >= 120, "connect window {total}s is too short for a restart");
        // and bounded, so a genuinely dead daemon does not burn a night
        assert!(total <= 600, "connect window {total}s waits too long on a dead daemon");
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
