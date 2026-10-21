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

use harmonia_protocol::daemon_wire::types2::{
    BuildMode, BuildResultInner, FailureStatus, SuccessStatus,
};
use harmonia_protocol::store_path::StoreDir;
use harmonia_protocol::types::{DaemonError, DaemonErrorKind, DaemonStore};
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::{Derivation, DerivationInputs};
use harmonia_store_derivation::derived_path::{
    DerivedPath, OutputName, OutputSpec, SingleDerivedPath,
};
use harmonia_store_path::{StorePath, StorePathSet};
use harmonia_store_remote::{ConnectionPool, PoolConfig, PooledConnectionGuard};
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
    /// `transient`: the daemon called the failure possibly transient (or did
    /// not say), so a later request may build it.
    #[error("build of {path} failed: {error_msg}")]
    BuildFailed {
        path: String,
        error_msg: String,
        transient: bool,
    },
    #[error("daemon returned no build result for {0}")]
    MissingBuildResult(String),
    // The variant name leads the Display text deliberately: the shell
    // monitors grep the LOG for "DaemonStalled", and the log only ever
    // sees this error through Display - a message without the name is a
    // stall the monitoring cannot match.
    // THE TEXT NAMED A CAUSE THIS SITE CANNOT ESTABLISH. `connect_failures`
    // counts only where `pool.acquire` fails, so a worker that ACCEPTS the
    // connection and then dies leaves it at zero and arrives here as a
    // connection-level IO error; nothing waited, and four crashed workers
    // exhaust the counter in under a second. "daemon-side wedge" and a
    // "final allowance" of 4800s together read as a daemon that was busy for
    // eighty minutes, which is what four aborts of one gate were attributed
    // to across three sessions while the daemon log recorded segfaults.
    // The allowance is the ladder's current rung, not a duration anything
    // spent, and the message now says where the discriminating evidence is.
    #[error(
        "DaemonStalled: no useful daemon reply through {attempts} stall attempts \
         and {connect_failures} connect failures (allowance ladder at \
         {last_allowance_s}s, a rung rather than a duration anything waited). \
         A connect that SUCCEEDS and then loses its worker arrives here too, so \
         this is not evidence the daemon was busy: read /var/log/nix-daemon/current \
         for 'daemon worker .* crashed' in this window before concluding load"
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

/// How long a caller keeps retrying a stalled realise, which is a property of
/// WHAT THE CALL IS FOR and cannot be read off the paths.
///
/// The escalating schedule exists for work a build cannot proceed without: a
/// task's inputs are worth 185 minutes of retry because the alternative is a
/// failed build. A pass whose documented failure mode is "the next run scans
/// as before" is not, and it was getting the same schedule - measured in a
/// live round, where the best-effort depfile collection held libcbor and
/// svt-av1 through a 300 s and a 1200 s allowance each, with 4800 s still to
/// come, to buy a cache whose absence costs a rescan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Patience {
    /// Essential work: 300s, 1200s, 4800s, then give up.
    Escalating,
    /// Best effort: one allowance, then report and move on.
    Single,
}

/// Watchdog allowance for the Nth stall retry: 300s, 1200s, 4800s, then
/// 4800s again for the final attempt. The `.min(2)` cap is load-bearing:
/// without it a large attempt count shifts past the u64 width.
/// Stalls a caller tolerates before the realise is abandoned.
fn patience_limit(patience: Patience) -> u32 {
    match patience {
        Patience::Escalating => 3,
        Patience::Single => 0,
    }
}

fn stall_allowance_s(stall_attempts: u32) -> u64 {
    300u64 << (2 * stall_attempts.min(2))
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bound a daemon exchange that is not a build. The build watchdog in
/// `build_paths_uncached` covered one of the client's six exchanges, and
/// a driver that starts fresh for every compile (the compiler drop-in)
/// spends its daemon time in the other five: NAR uploads of every input,
/// the derivation upload, the text uploads. Twelve such drivers sat asleep
/// for three hours inside a wedged daemon with nothing printed, while the
/// ninja-route drivers, already past their uploads, aborted DaemonStalled
/// on schedule. One allowance and no retry: an upload streams from a
/// producer that cannot be restarted, and turning an unbounded wait into
/// a named failure is the whole gap. Dropping the timed-out future leaves
/// the guard dirty, so the pool discards the connection and the daemon
/// child behind it is killed, which is what frees its locks.
async fn bounded<T>(
    what: &str,
    allowance: std::time::Duration,
    fut: impl std::future::Future<Output = std::result::Result<T, DaemonError>>,
) -> Result<T> {
    match tokio::time::timeout(allowance, fut).await {
        Ok(out) => out.map_err(Error::from),
        Err(_elapsed) => {
            eprintln!(
                "nix-ninja: [{}] WATCHDOG timeout on {what}: no daemon reply in {}s; \
                 dropped the connection",
                now_s(),
                allowance.as_secs(),
            );
            Err(Error::DaemonStalled {
                attempts: 1,
                connect_failures: 0,
                last_allowance_s: allowance.as_secs(),
            })
        }
    }
}

/// Milliseconds since this process first asked, for activity stamps.
fn now_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Runs FUT to completion unless no daemon frame arrives for ALLOWANCE:
/// LAST holds the `now_ms` of the latest frame and the caller stamps it.
/// `Err(())` is that silence. A realise is one call however large its
/// closure, and the daemon reports each build it starts and finishes on the
/// connection, so a wall-clock bound over the whole call dropped a working
/// connection: qtwebengine's assert_grd realise built 128 derivations of
/// its closure in the five minutes before its 300 s allowance ran out, the
/// drop interrupted the rest, and the retry failed "1 dependency failed".
async fn idle_timeout<T>(
    allowance: std::time::Duration,
    last: &std::sync::atomic::AtomicU64,
    fut: impl std::future::Future<Output = T>,
) -> std::result::Result<T, ()> {
    tokio::pin!(fut);
    let limit = allowance.as_millis() as u64;
    let tick = std::time::Duration::from_millis((limit / 4).clamp(10, 5000));
    loop {
        tokio::select! {
            out = &mut fut => return Ok(out),
            _ = tokio::time::sleep(tick) => {
                let quiet = now_ms().saturating_sub(last.load(std::sync::atomic::Ordering::Relaxed));
                if quiet >= limit {
                    return Err(());
                }
            }
        }
    }
}

/// The allowance every non-build exchange gets: the first rung of the
/// build watchdog's ladder.
fn upload_allowance() -> std::time::Duration {
    std::time::Duration::from_secs(stall_allowance_s(0))
}

/// A connection pool whose every acquisition carries a deadline.
///
/// `ConnectionPool::acquire` waits for a free slot INDEFINITELY, and it sits
/// BEFORE any watchdog the caller arms - so a bare acquire is an unbounded
/// wait the logging cannot see. Production code reaches the pool ONLY
/// through this wrapper, which makes a bare acquire a type error rather
/// than a property a test has to count occurrences of. (That test was
/// written first, and it failed four times on its own needles: the fix's
/// name contains the counted substring, one call spans a line break, and
/// the assertion message matched itself.) The one exception is the reaper's
/// `close_idle`, which holds the inner pool and bounds its own acquire.
///
/// THE ACQUIRE WAS THE ONE UNBOUNDED WAIT. A pool connection is held for
/// the whole of a build, so a request that finds every connection busy
/// waits on the pool rather than on the daemon, and the timeout around
/// `guard.execute` has not started yet when it parks. Measured 2026-10-15:
/// a peer's round burned one stall of 43 minutes with the driver's CPU
/// frozen at 00:02:32 and NO watchdog line at all - only the periodic
/// peak-in-flight - while the main thread sat in `futex_do_wait` and two
/// tokio workers sat on futexes beside it. The daemon's own log named the
/// cause at the matching cadence: `entering sandbox user namespace: Bad
/// file descriptor`, a child that dies holding its connection, so every
/// later request queues behind a guard that will never be released.
///
/// A TIMEOUT HERE IS NOT THE SAME EVENT AS A TIMEOUT ON THE EXCHANGE, and
/// the message says which: no request was sent, so there is no wedged
/// exchange to drop (a connect cut off mid-handshake is discarded by the
/// pool with the timed-out future). What the caller gets is
/// the same `DaemonStalled`, because the caller's recovery is the same -
/// retry on a fresh connection, which the build loop already does and which
/// re-reads the pool.
///
/// The other fields: the `now_ms` of the latest release, which the reaper
/// reads; the count of releases; the release quota after which a
/// connection is closed rather than parked (0 for never); and whether the
/// pool is reaped, which is also whether `acquire_retrying` retries.
struct BoundedPool(
    ConnectionPool,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
    u64,
    bool,
);

/// Close the connections parked idle in POOL, stopping as soon as a release
/// moves LAST past SEEN or nothing is left parked, and return how many
/// acquires here closed one. A parked connection already past the pool's
/// idle expiry is discarded by the pop itself and not counted. A request
/// that takes or releases a connection between the check and the acquire
/// can make this close a just-released connection or a fresh one it
/// connected, costing one fork. Bounded, so a pool whose every slot is held
/// cannot hold the reaper.
async fn close_idle(
    pool: &ConnectionPool,
    last: &std::sync::atomic::AtomicU64,
    seen: u64,
) -> usize {
    let mut closed = 0;
    while pool.stats().await.0 > 0 && last.load(std::sync::atomic::Ordering::Relaxed) == seen {
        match tokio::time::timeout(std::time::Duration::from_millis(500), pool.acquire()).await {
            Ok(Ok(guard)) => {
                guard.mark_broken();
                closed += 1;
            }
            _ => break,
        }
    }
    closed
}

/// Connections the reaper's acquires closed, for the driver's stats line. A
/// parked connection the pool's idle expiry discards during the pop is not
/// counted, and a fresh one the reaper connected after a race is.
pub static REAPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Released connections closed for reaching the quota, for the stats line.
/// A release whose exchange failed is counted too, though the pool would
/// have discarded that connection anyway.
pub static RECYCLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a host-route pool must go without a release before the reaper
/// closes its parked connections.
///
/// On the host route (a driver outside any build) the daemon forks one
/// worker process per connection, and a parked connection keeps its worker
/// alive. The pool expires an idle connection only when a later acquire
/// pops it, so a driver resolving on its own CPU held every worker it had
/// opened, idle, at whatever RSS each had reached. Closing on every release
/// instead costs a fork per exchange, each logged by the daemon. Reaping
/// after a quiet spell keeps reuse inside a burst of exchanges and still
/// lets the workers go when the driver stops asking.
///
/// Inside a build the socket is the build's own daemon, one thread per
/// connection in one process, and none of this is measured there, so the
/// pool keeps its default behaviour on that route.
///
/// A path an exchange added or built loses its temp root when the worker
/// that added it exits, and a retired worker exits while the driver still
/// runs, so this is safe only where no collector (min-free auto-GC
/// included) runs beside a host-route drive.
const REAP_QUIET: std::time::Duration = std::time::Duration::from_secs(2);

/// Every this many releases on the host route, the released connection is
/// closed rather than parked. A host-route worker grows while it serves,
/// not only while it idles: on one v8 class a driver reaping idle
/// connections alone saw its six workers grow from 0.31 to 8.82 GiB
/// (summed RSS) in two minutes of busy exchanges. Retiring every Nth
/// release gives each connection about N exchanges on average before its
/// worker exits, at one fork per N exchanges, so a busy driver's workers
/// do not keep what they grew for the rest of the drive. It does not cap
/// the peak: a few heavy exchanges grow a worker before its quota comes.
/// Prime, so that under a regular
/// release order the retirements do not fall on the same few of a pool's
/// connections every time.
const RECYCLE_EVERY: u64 = 499;

/// The host route's pool configuration for a reaper with quiet spell QUIET.
/// A trickle of exchanges never lets the pool go quiet. Expiring a parked
/// connection idle past twice the quiet spell on pop trims the pool under a
/// slow trickle, but the pool reuses first in, first out, so a serial
/// trickle every T seconds keeps about 2 * QUIET / T + 1 connections fresh,
/// and a trickle faster than that keeps the whole pool; the release quota
/// retires those. Twice the spell keeps connections parked together clear
/// of the reaper, which fires between one and one and a half spells after
/// the last release, so the reaper finds them rather than the pop
/// discarding them.
fn host_route_config(max_size: usize, quiet: std::time::Duration) -> PoolConfig {
    PoolConfig {
        max_size,
        max_idle_time: 2 * quiet,
        ..PoolConfig::default()
    }
}

/// A pooled connection that stamps the pool's release time when dropped,
/// and closes the connection instead of parking it on every quota-th
/// release.
struct Lease(
    Option<PooledConnectionGuard>,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
    u64,
);

impl std::ops::Deref for Lease {
    type Target = PooledConnectionGuard;
    fn deref(&self) -> &PooledConnectionGuard {
        self.0.as_ref().expect("lease is held until drop")
    }
}

impl std::ops::DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut PooledConnectionGuard {
        self.0.as_mut().expect("lease is held until drop")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        // Stamped before the connection is parked, which narrows, but does
        // not close, the window in which a reaper that read the old stamp
        // pops this connection (see `close_idle`).
        self.1.store(now_ms(), Relaxed);
        let n = self.2.fetch_add(1, Relaxed) + 1;
        match self.0.take() {
            Some(guard) if self.3 > 0 && n.is_multiple_of(self.3) => {
                guard.mark_broken();
                RECYCLED.fetch_add(1, Relaxed);
            }
            other => drop(other),
        }
    }
}

impl BoundedPool {
    fn new(path: &Path, config: PoolConfig) -> Self {
        Self(
            ConnectionPool::new(path, config),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(now_ms())),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            0,
            false,
        )
    }

    /// A pool whose parked connections are closed once no lease has been
    /// released for QUIET, and whose every EVERY-th released connection is
    /// closed. The reaper runs on RT for the pool's lifetime.
    fn new_reaped(
        path: &Path,
        config: PoolConfig,
        rt: &Runtime,
        quiet: std::time::Duration,
        every: u64,
    ) -> Self {
        let mut pool = Self::new(path, config);
        pool.3 = every;
        pool.4 = true;
        let (inner, last) = (pool.0.clone(), pool.1.clone());
        let tick = (quiet / 2).max(std::time::Duration::from_millis(50));
        rt.spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let seen = last.load(std::sync::atomic::Ordering::Relaxed);
                if now_ms().saturating_sub(seen) >= quiet.as_millis() as u64 {
                    let n = close_idle(&inner, &last, seen).await;
                    REAPED.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
        pool
    }

    /// Hold a slot WITHOUT a deadline, for the test that proves a full
    /// pool reports instead of parking. Production has no path to this:
    /// it exists so the arm can construct the state the defect produces -
    /// every connection held - against the same pool the bound call uses.
    #[cfg(test)]
    async fn hold_raw(&self) -> std::result::Result<PooledConnectionGuard, DaemonError> {
        self.0.acquire().await
    }

    async fn acquire(&self, what: &str, allowance: std::time::Duration) -> Result<Lease> {
        match tokio::time::timeout(allowance, self.0.acquire()).await {
            Ok(out) => out
                .map(|g| Lease(Some(g), self.1.clone(), self.2.clone(), self.3))
                .map_err(Error::from),
            Err(_elapsed) => {
                eprintln!(
                    "nix-ninja: [{}] WATCHDOG timeout on {what}: no connection in {}s \
                     (every slot held by a build that has not returned, or the connect \
                     itself did not finish); no request sent",
                    now_s(),
                    allowance.as_secs(),
                );
                Err(Error::DaemonStalled {
                    attempts: 1,
                    connect_failures: 0,
                    last_allowance_s: allowance.as_secs(),
                })
            }
        }
    }

    /// `acquire` with the build loop's connect ladder, for the exchanges
    /// outside the build loop, on a reaped pool only. An exchange after a
    /// reap connects afresh, so a refused or timed-out connect (a daemon
    /// restart, a busy accept after a wedge recovery) would otherwise fail
    /// the driver outright; a permanent connect error fails after six
    /// retries and 185 s of backoff, plus each attempt's connect timeout. A
    /// pool-wait stall passes straight through: it is not a connect. Safe to
    /// retry because the retry comes before any request is sent. It takes no
    /// retry gate: the build loop holds its gate through a whole build, so a
    /// shared gate would park an upload's retry behind it with nothing
    /// printed. An unreaped pool (inside a build) keeps the single attempt.
    async fn acquire_retrying(&self, what: &str, allowance: std::time::Duration) -> Result<Lease> {
        if !self.4 {
            return self.acquire(what, allowance).await;
        }
        let mut failures = 0u32;
        loop {
            match self.acquire(what, allowance).await {
                Err(e) if !matches!(e, Error::DaemonStalled { .. }) => {
                    failures += 1;
                    if failures > CONNECT_RETRIES {
                        return Err(e);
                    }
                    let wait =
                        CONNECT_BACKOFF_S[(failures as usize - 1).min(CONNECT_BACKOFF_S.len() - 1)];
                    eprintln!(
                        "nix-ninja: [{}] WATCHDOG connect-fail on {what} ({e}); retrying in \
                         {wait}s (connect failures {failures}/{CONNECT_RETRIES})",
                        now_s(),
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod bounded_exchange_tests {
    use super::{bounded, Error};

    #[test]
    fn a_silent_exchange_ends_as_daemon_stalled() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let got = rt.block_on(bounded::<()>(
            "probe",
            std::time::Duration::from_millis(20),
            std::future::pending(),
        ));
        assert!(
            matches!(got, Err(Error::DaemonStalled { attempts: 1, .. })),
            "{got:?}"
        );
    }

    /// Every daemon exchange except the build's own watchdog loop goes
    /// through `bounded`; a new `execute` site added without it reopens
    /// the unbounded wait this module exists to close.
    #[test]
    fn every_exchange_but_the_build_is_bounded() {
        let src = include_str!("lib.rs");
        // Needles assembled at run time so this test's own text is not a
        // match.
        let executes = src.matches(&[".execute(", "|client|"].concat()).count();
        let bounded_calls = src.matches(&["bounded(", "\n"].concat()).count();
        assert_eq!(
            executes,
            bounded_calls + 1,
            "execute sites {executes}, bounded {bounded_calls}"
        );
    }

    /// THE ACQUIRE IS THE WAIT THE WATCHDOG CANNOT SEE, and it is now a
    /// TYPE rather than a count. `BuilderRpcClient::pool` is a
    /// `BoundedPool`, whose only acquisition takes a deadline, so a bare
    /// `pool.acquire()` does not compile and no test has to look for one.
    ///
    /// THE COUNTING VERSION WAS TRIED FIRST AND FAILED FOUR TIMES, each on
    /// its own needle: the fix's name contains the counted substring
    /// (`acquire_bounded(` contains `bounded(`), one production call spans
    /// a line break, the assertion message matched itself, and the test
    /// modules are INTERLEAVED with production code so cutting at the
    /// first `#[cfg(test)]` deleted every call site and read a confident
    /// zero. A property the compiler enforces needs none of that.
    ///
    /// This arm states the guarantee in the one place a reader looks for
    /// it, and fails if the field's type is widened back.
    #[test]
    fn the_client_holds_a_bounded_pool() {
        let src = include_str!("lib.rs");
        let field = ["    pool: ", "BoundedPool,\n"].concat();
        assert!(
            src.contains(&field),
            "BuilderRpcClient::pool must stay a BoundedPool: a bare \
             ConnectionPool::acquire waits forever"
        );
    }

    /// THE BOUND FIRES ON THE REAL POOL, not on a stub: a one-slot pool
    /// whose only connection is held by another guard leaves `acquire`
    /// with no free slot and nothing to connect, so it parks exactly as
    /// it does when every connection is held by a build that has not
    /// returned - the state the defect produces - and the timeout is the
    /// only thing that ends it.
    ///
    /// A POOL OF ZERO IS NOT THE SAME THING: the constructor asserts
    /// positive capacity, so zero is unreachable and testing it proves
    /// nothing. A test that mocked the pool would prove the timeout wraps
    /// a future, which was never in doubt.
    ///
    /// NEEDS THE MACHINE'S DAEMON for the held guard to exist: with no
    /// socket the first acquire fails at connect and nothing is held, so
    /// where none is listening the arm is skipped rather than failed.
    #[test]
    fn a_pool_with_no_free_slot_ends_as_daemon_stalled() {
        use super::{BoundedPool, Error};
        use harmonia_store_remote::PoolConfig;

        let socket = std::path::Path::new("/nix/var/nix/daemon-socket/socket");
        if !socket.exists() {
            eprintln!("SKIP: no daemon socket, nothing to hold the slot with");
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = BoundedPool::new(
                socket,
                PoolConfig {
                    max_size: 1,
                    ..PoolConfig::default()
                },
            );
            // Held against the SAME pool the bound call uses: with the
            // only slot taken, `acquire` has nothing to grant and nothing
            // to connect, which is the state the defect produces.
            let held = match pool.hold_raw().await {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("SKIP: daemon not answering ({e:?}), nothing to hold");
                    return;
                }
            };
            let got = pool
                .acquire("probe", std::time::Duration::from_millis(20))
                .await;
            drop(held);
            // Matched rather than formatted: the Ok arm holds a guard that
            // is not Debug, and the subject is the error.
            match got {
                Err(Error::DaemonStalled { attempts: 1, .. }) => {}
                Err(other) => panic!("expected DaemonStalled, got {other:?}"),
                Ok(_) => panic!("acquire returned while the only slot was held"),
            }
        });
    }

    /// The control for the arm above: the same call with an allowance that
    /// has not elapsed must NOT report, or the arm is satisfied by a
    /// helper that always fails. Driven with a pool that HAS a slot, so
    /// the only difference from the subject is the state.
    #[test]
    fn a_pool_with_a_free_slot_does_not_report_a_stall() {
        use super::BoundedPool;
        use harmonia_store_remote::PoolConfig;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // A real daemon is needed for this arm to have a slot to hand
            // out; where none is listening the pool cannot connect and the
            // arm has nothing to say, so it is skipped rather than failed.
            if !std::path::Path::new("/nix/var/nix/daemon-socket/socket").exists() {
                return;
            }
            let pool = BoundedPool::new(
                std::path::Path::new("/nix/var/nix/daemon-socket/socket"),
                PoolConfig {
                    max_size: 1,
                    ..PoolConfig::default()
                },
            );
            let got = pool
                .acquire("probe", std::time::Duration::from_secs(30))
                .await;
            assert!(
                got.is_ok(),
                "a free slot must be granted, not reported as a stall"
            );
        });
    }

    /// THE REAPER CLOSES EVERY PARKED CONNECTION ONCE THE POOL GOES QUIET,
    /// AND NONE WHILE RELEASES KEEP COMING. An idle expiry inside the
    /// reaper's window would discard the parked connections uncounted and
    /// make the reaper connect a fresh one to close; two parked connections and the
    /// reaper's own count tell those apart, where one could not. The
    /// configuration is production's own (`host_route_config`). The busy
    /// readings are the controls, and they also fail a reaper that ignores
    /// the quiet gate or a lease that never stamps it.
    #[test]
    fn idle_connections_are_reaped_after_a_quiet_spell() {
        use super::{host_route_config, BoundedPool, REAPED};
        use std::sync::atomic::Ordering::Relaxed;
        use std::time::Duration;

        let socket = std::path::Path::new("/nix/var/nix/daemon-socket/socket");
        if !socket.exists() {
            eprintln!("SKIP: no daemon socket");
            return;
        }
        let quiet = Duration::from_secs(2);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        // The only reaper with a short quiet spell on a live socket, so
        // nothing else moves REAPED while this arm reads it; the route
        // test's reaper points at a missing socket and has nothing parked.
        let pool = BoundedPool::new_reaped(socket, host_route_config(2, quiet), &rt, quiet, 0);
        rt.block_on(async {
            let (a, b) = match (
                pool.acquire("probe", Duration::from_secs(30)).await,
                pool.acquire("probe", Duration::from_secs(30)).await,
            ) {
                (Ok(a), Ok(b)) => (a, b),
                _ => {
                    eprintln!("SKIP: daemon not answering");
                    return;
                }
            };
            drop(a);
            drop(b);
            let before = REAPED.load(Relaxed);
            // Twelve releases 400 ms apart outlast the quiet spell twice
            // over, so a reaper that ignores the gate, or a lease that never
            // stamps it, closes a connection in here. Read AFTER each pause:
            // a premature close lands between releases, on a reaper tick.
            // At the reaper's latest tick the older connection has idled
            // 3.4 s against the 4 s expiry, the margin a loaded machine gets.
            for i in 0..12 {
                let lease = pool
                    .acquire("probe", Duration::from_secs(30))
                    .await
                    .expect("a parked connection");
                drop(lease);
                tokio::time::sleep(Duration::from_millis(400)).await;
                let (idle, active, _) = pool.0.stats().await;
                assert_eq!(
                    (idle, active),
                    (2, 0),
                    "release {i}: a busy pool keeps its connections"
                );
            }
            tokio::time::sleep(Duration::from_millis(3500)).await;
            let (idle, active, _) = pool.0.stats().await;
            assert_eq!(
                (idle, active),
                (0, 0),
                "a quiet pool's parked connections are closed"
            );
            assert_eq!(
                REAPED.load(Relaxed) - before,
                2,
                "the reaper closed the two parked connections itself"
            );
        });
    }

    /// AN UNREAPED POOL (INSIDE A BUILD) DOES NOT RETRY A FAILED CONNECT:
    /// its first refusal comes straight back, where a reaped pool would
    /// wait 5 s before its first retry. No daemon is needed, since a socket
    /// that does not exist refuses at once.
    #[test]
    fn an_unreaped_pool_does_not_retry_a_connect() {
        use super::BoundedPool;
        use harmonia_store_remote::PoolConfig;
        use std::time::{Duration, Instant};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pool = BoundedPool::new(
            std::path::Path::new("/nonexistent/nn-no-daemon.sock"),
            PoolConfig {
                max_size: 1,
                ..PoolConfig::default()
            },
        );
        let started = Instant::now();
        let got = rt.block_on(pool.acquire_retrying("probe", Duration::from_secs(30)));
        assert!(got.is_err(), "a missing socket cannot connect");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an unreaped pool retried: {:?}",
            started.elapsed()
        );
    }

    /// A REAPED POOL (THE HOST ROUTE) DOES RETRY: on the same missing
    /// socket it is still in its backoff two seconds later, where the
    /// unreaped pool above had already returned.
    #[test]
    fn a_reaped_pool_retries_a_connect() {
        use super::BoundedPool;
        use harmonia_store_remote::PoolConfig;
        use std::time::Duration;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let pool = BoundedPool::new_reaped(
            std::path::Path::new("/nonexistent/nn-no-daemon.sock"),
            PoolConfig {
                max_size: 1,
                ..PoolConfig::default()
            },
            &rt,
            Duration::from_secs(600),
            0,
        );
        let got = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(2),
                pool.acquire_retrying("probe", Duration::from_secs(30)),
            )
            .await
        });
        assert!(got.is_err(), "a reaped pool returned instead of retrying");
    }

    /// THE ROUTE DECIDES THE POOL: a client inside a build gets the default
    /// pool, one on the host route the reaped one. Read off the client the
    /// production constructor builds; it connects nothing.
    #[test]
    fn only_the_host_route_reaps() {
        let path = std::path::Path::new("/nonexistent/nn-no-daemon.sock");
        let in_build = super::BuilderRpcClient::connect_unix_sized(path, true, 2).unwrap();
        let host = super::BuilderRpcClient::connect_unix_sized(path, false, 2).unwrap();
        assert!(!in_build.pool.4 && in_build.pool.3 == 0, "inside a build");
        assert!(
            host.pool.4 && host.pool.3 == super::RECYCLE_EVERY,
            "host route"
        );
    }

    /// EVERY QUOTA-TH RELEASE CLOSES ITS CONNECTION AND EVERY OTHER PARKS
    /// IT. A one-slot pool makes each reading exact: after a retiring
    /// release nothing is parked, after any other the one connection is.
    #[test]
    fn a_busy_connection_is_retired_after_its_quota() {
        use super::BoundedPool;
        use harmonia_store_remote::PoolConfig;
        use std::time::Duration;

        let socket = std::path::Path::new("/nix/var/nix/daemon-socket/socket");
        if !socket.exists() {
            eprintln!("SKIP: no daemon socket");
            return;
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        // A quiet spell no test outlasts, so only the quota closes anything.
        let pool = BoundedPool::new_reaped(
            socket,
            PoolConfig {
                max_size: 1,
                ..PoolConfig::default()
            },
            &rt,
            Duration::from_secs(600),
            3,
        );
        rt.block_on(async {
            for n in 1..=9u64 {
                match pool.acquire("probe", Duration::from_secs(30)).await {
                    Ok(lease) => drop(lease),
                    Err(e) if n == 1 => {
                        eprintln!("SKIP: daemon not answering ({e:?})");
                        return;
                    }
                    Err(e) => panic!("release {n}: acquire failed after the first: {e:?}"),
                }
                let (idle, active, _) = pool.0.stats().await;
                let want = if n % 3 == 0 { 0 } else { 1 };
                assert_eq!((idle, active), (want, 0), "release {n} of quota 3");
            }
        });
    }
}

#[cfg(test)]
mod watchdog_policy_tests {
    use super::stall_allowance_s;

    /// A call that keeps reporting outlives its allowance; a silent one
    /// does not. The first is the assert_grd realise the wall-clock bound
    /// dropped.
    #[test]
    fn the_allowance_bounds_silence_not_the_call() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Duration;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let allowance = Duration::from_millis(150);
        let last = AtomicU64::new(super::now_ms());
        let busy = rt.block_on(super::idle_timeout(allowance, &last, async {
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                last.store(super::now_ms(), Ordering::Relaxed);
            }
            7
        }));
        assert_eq!(busy, Ok(7), "a call reporting every 50 ms ran 400 ms");
        let last = AtomicU64::new(super::now_ms());
        let silent = rt.block_on(super::idle_timeout(allowance, &last, async {
            tokio::time::sleep(Duration::from_millis(600)).await;
            7
        }));
        assert_eq!(
            silent,
            Err(()),
            "a call silent for 600 ms passed a 150 ms allowance"
        );
    }

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
        // THE MONITORED SUBSTRING IS NOT THE ONLY THING THIS PINS. The
        // message exists to stop a reader concluding the daemon was busy,
        // so the pointer to the evidence is part of the contract: a reword
        // that drops it puts the misdiagnosis back.
        assert!(line.contains("/var/log/nix-daemon/current"), "{line}");
        assert!(
            !line.contains("wedge"),
            "the text asserts a cause this site cannot establish: {line}"
        );
    }

    /// The polarity that motivated `Patience`: a best-effort call must not
    /// be entitled to the escalation an essential one gets. Asserted as the
    /// TOTAL each is willing to wait, because that is the number the round
    /// pays - 185 minutes against 5.
    #[test]
    fn a_best_effort_call_gets_one_allowance() {
        use super::{patience_limit, stall_allowance_s, Patience};
        assert_eq!(patience_limit(Patience::Single), 0);
        assert_eq!(patience_limit(Patience::Escalating), 3);

        let total = |p| (0..=patience_limit(p)).map(stall_allowance_s).sum::<u64>();
        assert_eq!(total(Patience::Single), 300);
        assert_eq!(total(Patience::Escalating), 300 + 1200 + 4800 + 4800);
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
    pool: BoundedPool,
    /// The pool's connection count, so a caller fanning out requests never
    /// queues more of them than there are connections to carry them.
    pool_max: usize,
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

    /// Derivation -> the build failure the daemon reported for it in this
    /// process. The daemon keeps no record of a failure, so without this
    /// every consumer that realises a failed producer builds it again: one
    /// qttools iteration built five failing tasks 23 times each. Only a
    /// `BuildFailed` is kept; a transport or daemon error is retried as
    /// before.
    failed: Mutex<HashMap<String, String>>,

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

/// `(entries, bytes)` held in the `uploaded_drvs` ATerm map, the process-
/// lifetime structure whose retention a 2026-10-11 protobuf leaf showed as
/// rss 90 -> 459 MiB against heap 83 MiB live. The map's own comment forbids
/// the extrapolated size figure (one was falsified by a falling RSS), so the
/// instrument reports the map's size directly: the next leaf log answers
/// whether the retained gap IS this map without a heap dump. The two memos
/// beside it (realise, nar) hold keys and store paths, not payload bytes,
/// and their byte cost is bounded by the same count.
pub fn uploaded_drv_bytes() -> (u64, u64) {
    (
        UPLOADED_DRV_ENTRIES.load(std::sync::atomic::Ordering::Relaxed),
        UPLOADED_DRV_BYTES.load(std::sync::atomic::Ordering::Relaxed),
    )
}

// Entries and bytes of `uploaded_drvs`, kept as atomics beside the map rather
// than summed under its lock on every read: the stats tick reads them and the
// map is written per task, so a lock-held sum would put the progress tick in
// contention with the very work it measures.
static UPLOADED_DRV_ENTRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UPLOADED_DRV_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

/// Indices of `paths` grouped by the DERIVATION they name, in first-seen
/// order; every opaque path is its own group. The groups partition
/// `0..paths.len()` and each group's indices are ascending, which is what
/// lets the caller write results back by index without a second pass.
/// `f(0..n)` on at most `width` threads, results in index order, the first
/// error by index returned. Extracted from `build_paths` so the fan-out is
/// tested without a daemon.
pub fn for_each_bounded<T: Send>(
    n: usize,
    width: usize,
    f: impl Fn(usize) -> Result<T> + Sync,
) -> Result<Vec<T>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<Result<T>>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..width.clamp(1, n.max(1)) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let r = f(i);
                let failed = r.is_err();
                slots.lock().unwrap()[i] = Some(r);
                if failed {
                    // Stop handing out work; requests in flight finish.
                    next.store(n, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    let mut out = Vec::with_capacity(n);
    for r in slots.into_inner().unwrap() {
        match r {
            Some(Ok(v)) => out.push(v),
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }
    Ok(out)
}

/// The error for the first of `misses` whose derivation already failed in
/// this run, if any.
fn failed_earlier(
    failed: &HashMap<String, String>,
    store_dir: &StoreDir,
    misses: &[SingleDerivedPath],
) -> Option<Error> {
    misses.iter().find_map(|m| {
        let drv = built_drv(store_dir, m)?;
        let why = failed.get(&drv)?;
        Some(Error::BuildFailed {
            error_msg: format!("failed earlier in this run, not built again: {why}"),
            path: drv,
            transient: false,
        })
    })
}

/// Records every derivation of `subset` as failed when `e` is a build
/// failure the daemon did not call transient.
fn note_build_failure(
    failed: &mut HashMap<String, String>,
    store_dir: &StoreDir,
    subset: &[SingleDerivedPath],
    e: &Error,
) {
    if let Error::BuildFailed {
        error_msg,
        transient: false,
        ..
    } = e
    {
        for m in subset {
            if let Some(drv) = built_drv(store_dir, m) {
                failed.entry(drv).or_insert_with(|| error_msg.clone());
            }
        }
    }
}

/// Whether a build failure with this status may pass on a later request:
/// the daemon's own "possibly transient", a timeout, or a failure it does
/// not classify.
fn failure_may_pass(status: &FailureStatus) -> bool {
    matches!(
        status,
        FailureStatus::TransientFailure | FailureStatus::TimedOut | FailureStatus::MiscFailure
    )
}

/// The derivation a derived path is built by, as the daemon names it.
fn built_drv(store_dir: &StoreDir, p: &SingleDerivedPath) -> Option<String> {
    match p {
        SingleDerivedPath::Built { drv_path, .. } => {
            Some(store_dir.display(drv_path.as_ref()).to_string())
        }
        SingleDerivedPath::Opaque(_) => None,
    }
}

fn split_by_derivation(store_dir: &StoreDir, paths: &[SingleDerivedPath]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        let key = match p {
            SingleDerivedPath::Opaque(path) => format!("opaque:{i}:{path}"),
            SingleDerivedPath::Built { drv_path, .. } => {
                format!("drv:{}", store_dir.display(drv_path.as_ref()))
            }
        };
        match index.get(&key) {
            Some(&g) => groups[g].push(i),
            None => {
                index.insert(key, groups.len());
                groups.push(vec![i]);
            }
        }
    }
    groups
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

/// Derivations to report if the daemon builds or substitutes them (a later
/// pass's reused tasks, whose outputs should already be valid), and those
/// it did.
static WATCHED: std::sync::Mutex<Option<(std::collections::HashSet<String>, Vec<String>)>> =
    std::sync::Mutex::new(None);

/// Reports `drv` in `built_watched` if a realise builds or substitutes it.
pub fn watch_built(drv: String) {
    WATCHED
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .0
        .insert(drv);
}

/// The watched derivations a realise built or substituted, each once.
pub fn built_watched() -> Vec<String> {
    let mut built = WATCHED
        .lock()
        .unwrap()
        .as_ref()
        .map(|w| w.1.clone())
        .unwrap_or_default();
    built.sort();
    built.dedup();
    built
}

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
mod split_by_derivation_tests {
    use super::split_by_derivation;
    use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
    use harmonia_store_path::StoreDir;
    use std::str::FromStr;
    use std::sync::Arc;

    fn built(sd: &StoreDir, drv: &str, out: &str) -> SingleDerivedPath {
        SingleDerivedPath::Built {
            drv_path: Arc::new(SingleDerivedPath::Opaque(sd.parse(drv).unwrap())),
            output: OutputName::from_str(out).unwrap(),
        }
    }

    /// Two derivations never share a request; two outputs of one do; an
    /// opaque path is its own group. Order is first-seen and every index
    /// lands in exactly one group, which is what the write-back relies on.
    #[test]
    fn two_derivations_are_two_groups_and_one_derivation_stays_batched() {
        let sd = StoreDir::new("/nix/store").unwrap();
        let a = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fail-a.drv";
        let b = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-fail-b.drv";
        let paths = vec![
            built(&sd, a, "out"),
            built(&sd, b, "out"),
            built(&sd, a, "dev"),
            SingleDerivedPath::Opaque(
                sd.parse("/nix/store/cccccccccccccccccccccccccccccccc-src")
                    .unwrap(),
            ),
        ];
        let groups = split_by_derivation(&sd, &paths);
        assert_eq!(groups, vec![vec![0, 2], vec![1], vec![3]]);
        let mut all: Vec<usize> = groups.concat();
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2, 3], "a partition of the request");
    }

    /// THE COMMON CASE IS ONE GROUP, and the caller keeps the single
    /// request for it, so a per-task realise is unchanged by the split.
    #[test]
    fn a_single_derivation_request_is_one_group() {
        let sd = StoreDir::new("/nix/store").unwrap();
        let a = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one.drv";
        let groups = split_by_derivation(&sd, &[built(&sd, a, "out"), built(&sd, a, "out.d")]);
        assert_eq!(groups.len(), 1);
    }
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

/// Printed when `NIX_NINJA_RPC_HOST_ROUTE=1` overrides `NIX_BUILD_TOP`. The
/// consumer's preflight sweep matches this text to score a drive as keyed
/// the way the build keys it, so a change here is a change to that contract.
pub const HOST_ROUTE_NOTICE: &str =
    "nix-ninja: NIX_NINJA_RPC_HOST_ROUTE=1: host daemon route with NIX_BUILD_TOP set";

#[cfg(test)]
mod host_route_notice_tests {
    /// A ratchet, not an oracle: the consumer greps this exact text.
    #[test]
    fn the_host_route_notice_is_the_text_the_sweep_matches() {
        assert_eq!(
            super::HOST_ROUTE_NOTICE,
            "nix-ninja: NIX_NINJA_RPC_HOST_ROUTE=1: host daemon route with NIX_BUILD_TOP set"
        );
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
        //
        // NIX_NINJA_RPC_HOST_ROUTE=1 forces the host route with NIX_BUILD_TOP
        // set, so a driver run OUTSIDE a build can carry the build's own
        // NIX_BUILD_TOP and key what the build keys: the gcc wrapper's
        // preprocessor fallback reads it (`unbound variable` without it).
        let build_top = std::env::var_os("NIX_BUILD_TOP").is_some();
        let forced = std::env::var_os("NIX_NINJA_RPC_HOST_ROUTE").is_some_and(|v| v == "1");
        if build_top && forced {
            eprintln!("{HOST_ROUTE_NOTICE}");
        }
        let in_drv = build_top && !forced;

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
        // Reaping and recycling are measured on the host route only (see
        // REAP_QUIET); inside a build the pool keeps its default behaviour.
        let pool = if in_drv {
            BoundedPool::new(
                path,
                PoolConfig {
                    max_size: pool_max.max(1),
                    ..PoolConfig::default()
                },
            )
        } else {
            BoundedPool::new_reaped(
                path,
                host_route_config(pool_max.max(1), REAP_QUIET),
                &runtime,
                REAP_QUIET,
                RECYCLE_EVERY,
            )
        };
        Ok(Self {
            runtime,
            pool,
            pool_max: pool_max.max(1),
            in_drv,
            uploaded_drvs: Default::default(),
            realised: Default::default(),
            failed: Default::default(),
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
            let mut guard = self
                .pool
                .acquire_retrying(&format!("add_drv_to_store {name}"), upload_allowance())
                .await?;
            let source = BufReader::new(Cursor::new(bytes.clone()));
            bounded(
                &format!("add_drv_to_store {name}"),
                upload_allowance(),
                guard.execute(|client| {
                    client.add_ca_to_store(
                        &name,
                        ContentAddressMethodAlgorithm::Text,
                        &refs,
                        false,
                        source,
                    )
                }),
            )
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
            UPLOADED_DRV_ENTRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            UPLOADED_DRV_BYTES.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
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
                let what = format!("add_to_store_text {name}");
                let mut guard = self
                    .pool
                    .acquire_retrying(&what, upload_allowance())
                    .await?;
                let source = BufReader::new(Cursor::new(bytes));
                if self.in_drv {
                    bounded(
                        &what,
                        upload_allowance(),
                        guard.execute(|client| {
                            client.add_to_store_scanning(
                                name,
                                ContentAddressMethodAlgorithm::Text,
                                source,
                            )
                        }),
                    )
                    .await
                } else {
                    // Unfortunately outside of a derivation we do not have a good
                    // idea of possible paths for which we can scan.
                    // A trivial "scan for all paths" implementation would include nonexistent paths
                    // from documentation and fail on important projects, e.g. nix.
                    // An empty reference set is identical to the fallback `nix store add` case
                    // and preferable to no running outside a derivation at all.
                    let refs = Default::default();
                    bounded(
                        &what,
                        upload_allowance(),
                        guard.execute(|client| {
                            client.add_ca_to_store(
                                name,
                                ContentAddressMethodAlgorithm::Text,
                                &refs,
                                false,
                                source,
                            )
                        }),
                    )
                    .await
                }
            })
            .map_err(|e| Error::AddToStore {
                name: name.to_string(),
                detail: format!("text, {} bytes", bytes.len()),
                source: Box::new(e),
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
        timed_add(|| self.add_to_store_nar_cached_inner(name, path, key))
    }

    fn add_to_store_nar_cached_inner(
        &self,
        name: &str,
        path: &Path,
        key: &Path,
    ) -> Result<StorePath> {
        let t0 = std::time::Instant::now();
        let stamp = std::fs::metadata(key).ok().and_then(|md| {
            let mtime = md
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            Some((md.len(), mtime))
        });
        if let Some(sp) = nar_stamp_hit(&self.nar_uploads, key, stamp) {
            NAR_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ADD_HIT_US.fetch_add(
                t0.elapsed().as_micros() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            return Ok(sp);
        }
        NAR_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The INNER upload, not the timed public wrapper: this call is
        // already inside one, and calling the wrapper counted every miss
        // twice and nested its duration inside its own total. Measured on
        // two recorded runs, where the reported call count was exactly
        // hits + 2 * uploads.
        let sp = self.add_to_store_nar_inner(name, path)?;
        remember_nar_stamp(&self.nar_uploads, key, stamp, &sp);
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
    /// it. The exchange itself is never retried: `execute` is FnOnce and
    /// poisons its connection on error, and no caller retries this. Only the
    /// connect is (`acquire_retrying`), and the pipe is read only after it
    /// returns, so a retried connect loses no bytes.
    pub fn add_to_store_nar(&self, name: &str, path: &Path) -> Result<StorePath> {
        timed_add(|| self.add_to_store_nar_inner(name, path))
    }

    fn add_to_store_nar_inner(&self, name: &str, path: &Path) -> Result<StorePath> {
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
                let what = format!("add_to_store_nar {name}");
                let mut guard = self
                    .pool
                    .acquire_retrying(&what, upload_allowance())
                    .await?;
                let source = BufReader::new(reader);
                let out = if self.in_drv {
                    bounded(
                        &what,
                        upload_allowance(),
                        guard.execute(|client| {
                            client.add_to_store_scanning(
                                name,
                                ContentAddressMethodAlgorithm::NixArchive(
                                    harmonia_utils_hash::Algorithm::SHA256,
                                ),
                                source,
                            )
                        }),
                    )
                    .await
                } else {
                    // Suboptimal fallback, see add_to_store_text
                    let refs = Default::default();
                    bounded(
                        &what,
                        upload_allowance(),
                        guard.execute(|client| {
                            client.add_ca_to_store(
                                name,
                                ContentAddressMethodAlgorithm::NixArchive(
                                    harmonia_utils_hash::Algorithm::SHA256,
                                ),
                                &refs,
                                false,
                                source,
                            )
                        }),
                    )
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
                out
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
        patience: Patience,
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

        // ONE DERIVATION PER REQUEST, and the reason is a daemon bug this
        // client cannot see past. nix 2.36.0pre segfaults a worker in
        // DerivationBuilderImpl::cleanupBuild whenever ONE
        // BuildPathsWithResults names two or more derivations that both
        // FAIL: the goal set's teardown after the first failure clears a
        // pointer the second failed goal's cleanup then reads. Isolated
        // 2026-09-19 with examples/failing_build.rs, every arm read off the
        // daemon's crash count: one failing derivation of any kind never
        // crashes, two in one request always do, two through plain
        // `nix build --keep-going` do not. The retry arm below then costs
        // one worker per attempt, which is the four-in-a-row signature the
        // DaemonStalled abort reports as a wedge.
        //
        // So a request naming more than one DERIVATION is split by
        // derivation. Opaque paths and the outputs of one derivation stay
        // batched, since the merge below is what keys results correctly
        // for those. The cost is one round trip per derivation at the sites
        // that ask for several at once, which are the placement passes at
        // the end of a run; the per-task realise path asks for one.
        // A DERIVATION THAT ALREADY FAILED IN THIS RUN IS NOT BUILT AGAIN:
        // the same request fails with the same error, at once.
        if let Some(e) = failed_earlier(&self.failed.lock().unwrap(), store_dir, &misses) {
            return Err(e);
        }
        let note_failure = |subset: &[SingleDerivedPath], e: &Error| {
            note_build_failure(&mut self.failed.lock().unwrap(), store_dir, subset, e)
        };
        let groups = split_by_derivation(store_dir, &misses);
        let built = if groups.len() <= 1 {
            self.build_paths_uncached(store_dir, &misses, patience)
                .inspect_err(|e| note_failure(&misses, e))?
        } else {
            // THE GROUPS RUN CONCURRENTLY, ONE REQUEST EACH. Serial, the
            // end-of-run placement of a restarted round asked the daemon for
            // every banked output one derivation at a time on one connection
            // while the rest of the pool sat idle: three drivers at 0 CPU for
            // over 30 minutes with no task in flight (KDE round, 2026-10-17).
            // Each request still names one derivation, so the crash above
            // cannot be reached; width is the pool's, so no request waits on
            // an acquire deadline for a connection that cannot come.
            let results = for_each_bounded(groups.len(), self.pool_max, |g| {
                let subset: Vec<SingleDerivedPath> =
                    groups[g].iter().map(|&i| misses[i].clone()).collect();
                self.build_paths_uncached(store_dir, &subset, patience)
                    .inspect_err(|e| note_failure(&subset, e))
            })?;
            let mut by_slot: Vec<Option<StorePath>> = vec![None; misses.len()];
            for (group, got) in groups.iter().zip(results) {
                for (&i, sp) in group.iter().zip(got) {
                    by_slot[i] = Some(sp);
                }
            }
            by_slot
                .into_iter()
                .map(|o| o.expect("every miss belongs to exactly one group"))
                .collect()
        };
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
        patience: Patience,
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
                let allowance = std::time::Duration::from_secs(allowance_s);
                let mut guard = match self.pool.acquire(&waited_on, allowance).await {
                    Ok(g) => g,
                    Err(Error::DaemonStalled { .. }) => {
                        // The pool never granted a connection: every one is
                        // held by a build that has not returned. That is a
                        // STALL, not a failed connect, so it spends the
                        // stall budget with its escalating allowance, not
                        // the connect one. There is no guard to drop -
                        // nothing was sent - and the retry re-reads the pool.
                        stall_attempts += 1;
                        if stall_attempts > patience_limit(patience) {
                            break Err(Error::DaemonStalled {
                                attempts: stall_attempts,
                                connect_failures,
                                last_allowance_s: allowance_s,
                            });
                        }
                        eprintln!(
                            "nix-ninja: [{}] WATCHDOG pool-wait on {waited_on}: no \
                             pooled connection in {allowance_s}s; retrying \
                             (stalls this call {stall_attempts})",
                            now_s(),
                        );
                        continue;
                    }
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
                            break Err(e);
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
                // THE ALLOWANCE IS FOR SILENCE, NOT FOR THE CALL: every frame
                // the daemon sends while it builds (activity start, result,
                // stop, log lines) restamps `last`, so a long closure that is
                // building is never dropped, and a wedged daemon still is.
                let last = std::sync::atomic::AtomicU64::new(now_ms());
                let outcome = idle_timeout(
                    std::time::Duration::from_secs(allowance_s),
                    &last,
                    guard.execute(|client| async {
                        use futures::StreamExt as _;
                        let rl = client.build_paths_with_results(&merged, BuildMode::Normal);
                        tokio::pin!(rl);
                        while rl.next().await.is_some() {
                            last.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                        }
                        rl.await
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
                        // PATIENCE DOES NOT GATE THIS ARM, and gating it
                        // was a defect. A mid-execute connection error is
                        // not a stalled daemon: the common cause is a
                        // SIBLING's watchdog dropping a wedged connection,
                        // which this call recovers from by reconnecting and
                        // which costs no wall clock. Sharing `stall_attempts`
                        // with the timeout arm spent a best-effort call's one
                        // allowance on an event where nothing had waited, and
                        // reported it as DaemonStalled with attempts: 1.
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
                        // A BEST-EFFORT CALL GETS ONE ALLOWANCE. Its caller
                        // treats failure as a cache miss, so retrying spends
                        // the package's wall clock on work nobody waits for.
                        if stall_attempts > patience_limit(patience) {
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
                if let (
                    SuccessStatus::Built | SuccessStatus::Substituted,
                    DerivedPath::Built { drv_path, .. },
                ) = (success.status, &r.path)
                {
                    let shown = store_dir.display(drv_path.as_ref()).to_string();
                    if let Some(w) = WATCHED.lock().unwrap().as_mut() {
                        if w.0.contains(&shown) {
                            w.1.push(shown);
                        }
                    }
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
                                let (own, transient) = match &result.inner {
                                    BuildResultInner::Failure(f) => (
                                        String::from_utf8_lossy(&f.error_msg).into_owned(),
                                        failure_may_pass(&f.status),
                                    ),
                                    _ => (String::new(), true),
                                };
                                let extra = others(&display);
                                Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: format!("{own}{extra}"),
                                    transient,
                                })
                            }
                            _ => Ok(path.clone()),
                        }
                    }
                    SingleDerivedPath::Built { drv_path, output } => {
                        // A WATCHED DERIVATION WITH NO RESULT OF ITS OWN is
                        // counted as built. Nix keys every result by the
                        // path requested, so this does not happen; if a
                        // daemon ever replied otherwise, a count must fail
                        // toward work done.
                        if !by_key.contains_key(&key) {
                            let shown = store_dir.display(drv_path.as_ref()).to_string();
                            if let Some(w) = WATCHED.lock().unwrap().as_mut() {
                                if w.0.contains(&shown) {
                                    w.1.push(format!("{shown} (no result of its own)"));
                                }
                            }
                        }
                        if let Some(result) = by_key.get(&key) {
                            if result.success().is_none() {
                                let (own, transient) = match &result.inner {
                                    BuildResultInner::Failure(f) => (
                                        String::from_utf8_lossy(&f.error_msg).into_owned(),
                                        failure_may_pass(&f.status),
                                    ),
                                    _ => (String::new(), true),
                                };
                                let extra = others(&display);
                                return Err(Error::BuildFailed {
                                    path: display,
                                    error_msg: format!("{own}{extra}"),
                                    transient,
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
                                    transient: true,
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
            let what = format!("submit_output {name}");
            let mut guard = self
                .pool
                .acquire_retrying(&what, upload_allowance())
                .await?;
            bounded(
                &what,
                upload_allowance(),
                guard.execute(|client| client.submit_output(path, name)),
            )
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

/// Wall clock inside the file-add path, and the calls that spent it.
///
/// UPSTREAM #18 opens with a suspicion, that derivation generation is slowed
/// by adding almost every input file to the store synchronously, and nothing
/// here could confirm or refute it: the memo's hit and upload counts say how
/// often the daemon was reached and never how long it took. A change made on
/// the strength of the suspicion would have no number to be judged against
/// afterwards either.
///
/// Measured around the whole call rather than around the transfer, since a
/// cache hit is part of what the phase spends and an optimisation that only
/// moves misses off the critical path has to be visible as such.
///
/// THE TOTAL IS THREAD TIME AND NOT WALL CLOCK. Adds already run eight wide
/// under `new_opaque_files`, and each call adds its own elapsed time, so
/// concurrent calls each contribute in full and the sum can exceed the phase
/// that contains it. A figure larger than the generation window is evidence
/// that the adds are already concurrent, never that they are the wall clock
/// to be recovered.
///
/// The hit total is carried separately because the two halves have different
/// remedies and only their split says which is worth pursuing. A memo hit
/// never reaches the daemon: it stats the key and reads an in-process map, so
/// where the hits are cheap, spreading them across threads parallelises
/// almost nothing and the only route to the remaining time is making fewer
/// real uploads. Microseconds, since a hit that took a whole millisecond
/// would already be the answer.
static ADD_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ADD_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ADD_HIT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(milliseconds, calls, milliseconds on stamp-cache hits)` spent adding
/// files to the store. The third term is reported with the first two rather
/// than separately because the pair alone is the reading that misleads.
pub fn file_add_stats() -> (u64, u64, u64) {
    (
        ADD_MS.load(std::sync::atomic::Ordering::Relaxed),
        ADD_CALLS.load(std::sync::atomic::Ordering::Relaxed),
        ADD_HIT_US.load(std::sync::atomic::Ordering::Relaxed) / 1000,
    )
}

/// Times one file-add call into the counters above.
///
/// ONE CALL MUST PASS THROUGH ONE OF THESE. Every public entry point is
/// wrapped, so an entry point that reaches another one counts its work twice
/// and nests the inner duration inside the outer; the internal `*_inner`
/// spellings exist for exactly that reason and are what a wrapped path calls.
fn timed_add<T>(f: impl FnOnce() -> T) -> T {
    let t0 = std::time::Instant::now();
    let out = f();
    ADD_MS.fetch_add(
        t0.elapsed().as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    ADD_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    out
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
mod failure_memo_tests {
    use super::*;
    use harmonia_store_derivation::derived_path::OutputName;

    fn built(store_dir: &StoreDir, drv: &str) -> SingleDerivedPath {
        SingleDerivedPath::Built {
            drv_path: std::sync::Arc::new(SingleDerivedPath::Opaque(store_dir.parse(drv).unwrap())),
            output: OutputName::default(),
        }
    }

    fn failure(transient: bool) -> Error {
        Error::BuildFailed {
            path: String::new(),
            error_msg: "builder failed with exit code 1".into(),
            transient,
        }
    }

    /// A build failure is answered from the memo for the same derivation and
    /// no other; a transient one, or a transport error, is not kept.
    #[test]
    fn a_failed_derivation_is_not_built_again() {
        let sd = StoreDir::default();
        let a = built(&sd, "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv");
        let b = built(&sd, "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv");
        let mut failed = HashMap::new();
        note_build_failure(&mut failed, &sd, std::slice::from_ref(&a), &failure(true));
        note_build_failure(&mut failed, &sd, std::slice::from_ref(&a), &Error::NoDaemon);
        assert!(failed_earlier(&failed, &sd, std::slice::from_ref(&a)).is_none());
        note_build_failure(&mut failed, &sd, std::slice::from_ref(&a), &failure(false));
        let e = failed_earlier(&failed, &sd, &[b.clone(), a.clone()]).expect("a failed");
        assert!(
            matches!(&e, Error::BuildFailed { path, transient: false, .. } if path.ends_with("-a.drv")),
            "{e}"
        );
        assert!(failed_earlier(&failed, &sd, &[b]).is_none());
    }

    #[test]
    fn only_a_possibly_passing_status_is_transient() {
        assert!(failure_may_pass(&FailureStatus::TransientFailure));
        assert!(failure_may_pass(&FailureStatus::TimedOut));
        assert!(!failure_may_pass(&FailureStatus::PermanentFailure));
        assert!(!failure_may_pass(&FailureStatus::DependencyFailed));
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
        use super::remember_nar_stamp;
        use harmonia_store_path::StoreDir;
        use std::collections::HashMap;
        use std::sync::Mutex;

        let sd = StoreDir::new("/nix/store").unwrap();
        let sp = sd
            .parse("/nix/store/00000000000000000000000000000000-probe")
            .unwrap();
        let map: Mutex<HashMap<std::path::PathBuf, (u64, u128, _)>> = Mutex::new(HashMap::new());
        let key = std::path::Path::new("/nonexistent/nn-memo-probe");

        // The precondition, kept because it is what makes the case real:
        // this is the stamp an unstattable key produces.
        assert!(std::fs::metadata(key).is_err());

        remember_nar_stamp(&map, key, None, &sp);
        assert!(
            map.lock().unwrap().is_empty(),
            "an unstattable key was remembered; nar_stamps_snapshot would \
             persist it and every later run would seed an entry no lookup \
             can hit"
        );

        // And the positive half, so the arm cannot pass by never inserting.
        remember_nar_stamp(&map, key, Some((7, 9)), &sp);
        assert_eq!(map.lock().unwrap().len(), 1, "a stamped key must be kept");
    }
}

/// Record an upload against its key, ONLY when the key could be stamped.
///
/// An unstattable key must stay a miss forever rather than be remembered
/// under a stamp of zero. A zero-stamped entry can never be HIT - the hit
/// path needs a stamp too - so the damage is not a wrong answer: it is that
/// `nar_stamps_snapshot` persists the entry, the next run seeds it, and the
/// cache carries a growing set of entries no lookup can ever use.
///
/// A free function taking the map rather than a method, because the
/// enclosing upload needs a daemon and this decision does not. The arm that
/// covered this asserted that `fs::metadata` of a nonexistent path is an
/// error, which is a fact about the standard library: it called nothing in
/// this crate, so the guard could be deleted with it green.
/// The stamp-cache lookup, decided without a daemon.
///
/// Extracted for the same reason as `remember_nar_stamp` below: the
/// enclosing upload needs a connection and this decision does not, so a
/// test of the decision would otherwise have to assert around one.
///
/// It answers only on a stamp that matches in BOTH size and mtime. Size
/// alone would adopt a same-length rewrite of a file, which is the shape an
/// edit to a version string or a generated header takes; mtime alone would
/// adopt a restored copy. The `None` stamp is the unstattable key, which can
/// never hit, and is the case a test of the happy path does not reach.
fn nar_stamp_hit(
    map: &Mutex<HashMap<PathBuf, (u64, u128, StorePath)>>,
    key: &Path,
    stamp: Option<(u64, u128)>,
) -> Option<StorePath> {
    let (size, mtime) = stamp?;
    let guard = map.lock().unwrap();
    let (s, m, sp) = guard.get(key)?;
    (*s == size && *m == mtime).then(|| sp.clone())
}

fn remember_nar_stamp(
    map: &Mutex<HashMap<PathBuf, (u64, u128, StorePath)>>,
    key: &Path,
    stamp: Option<(u64, u128)>,
    sp: &StorePath,
) {
    if let Some((size, mtime)) = stamp {
        map.lock()
            .unwrap()
            .insert(key.to_path_buf(), (size, mtime, sp.clone()));
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
    // THE GROUP CLAUSE IS SCOPED THE WAY THE CGROUP ONE IS, and it was not.
    // `should not be a member of` alone is an unanchored substring, and this
    // function is handed daemon error text that CARRIES A FAILING BUILD'S OWN
    // OUTPUT, so a diagnostic using that phrase would be read as the nixbld
    // race. The polarity is the dangerous one: a false TRUE here suppresses a
    // verdict and retries a build that genuinely failed, where a false FALSE
    // only reports the race as terminal. Unwitnessed, and tightened because
    // the consumer hit the same shape in an audit arm whose `/nonexistent`
    // literal matched the English phrase `inactive/nonexistent` in a kernel
    // UAPI doc comment (2026-10-03). Nix's own text names the user and the
    // group; a compiler cannot.
    (msg.contains("/sys/fs/cgroup/")
        && (msg.contains("No such file") || msg.contains("File exists")))
        || (msg.contains("should not be a member of") && msg.contains("Nix user"))
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
        // THE GROUP PHRASE INSIDE A BUILD'S OWN OUTPUT IS NOT THE RACE. The
        // daemon's error text carries the failing build's log, so an
        // unanchored match on the phrase alone retried a real failure.
        assert!(!transient_daemon_error(
            "builder failed with exit code 1: error: 'x' should not be a member of \
             class Y [-Werror]"
        ));
    }
}

#[cfg(test)]
mod nar_stamp_hit_tests {
    use super::*;

    fn sp() -> StorePath {
        "00000000000000000000000000000000-x"
            .parse()
            .expect("fixture store path must parse")
    }

    fn map_with(
        key: &Path,
        size: u64,
        mtime: u128,
    ) -> Mutex<HashMap<PathBuf, (u64, u128, StorePath)>> {
        let mut m = HashMap::new();
        m.insert(key.to_path_buf(), (size, mtime, sp()));
        Mutex::new(m)
    }

    #[test]
    fn a_matching_stamp_hits() {
        let k = Path::new("/build/src/main.c");
        assert!(nar_stamp_hit(&map_with(k, 12, 34), k, Some((12, 34))).is_some());
    }

    #[test]
    fn a_same_length_rewrite_does_not_hit() {
        // The shape an edited version string takes: identical size, new mtime.
        // Size alone would adopt the stale store path and compile the old bytes.
        let k = Path::new("/build/version.h");
        assert!(nar_stamp_hit(&map_with(k, 12, 34), k, Some((12, 99))).is_none());
    }

    #[test]
    fn a_restored_copy_does_not_hit() {
        // Same mtime, different size: mtime alone would adopt it.
        let k = Path::new("/build/gen.c");
        assert!(nar_stamp_hit(&map_with(k, 12, 34), k, Some((99, 34))).is_none());
    }

    #[test]
    fn an_unstattable_key_cannot_hit_a_zero_stamped_entry() {
        // The zero case, and the entry is stamped ZERO on purpose. Stamped
        // anything else the assertion holds for the wrong reason, since the
        // comparison fails on the values and the missing stamp is never the
        // thing under test; a mutant substituting a zero default for the
        // absent stamp survived exactly that fixture. Here the default WOULD
        // match, so only refusing to compare at all keeps this green, which
        // is the guard remember_nar_stamp's note relies on.
        let k = Path::new("/build/vanished.c");
        assert!(nar_stamp_hit(&map_with(k, 0, 0), k, None).is_none());
    }

    #[test]
    fn an_absent_key_does_not_hit() {
        let k = Path::new("/build/known.c");
        assert!(nar_stamp_hit(
            &map_with(k, 12, 34),
            Path::new("/build/other.c"),
            Some((12, 34))
        )
        .is_none());
    }
}

#[cfg(test)]
mod fan_out_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fan_out_runs_wide_keeps_order_and_bounds_width() {
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let got = for_each_bounded(24, 6, |i| {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            live.fetch_sub(1, Ordering::SeqCst);
            Ok(i * 10)
        })
        .unwrap();
        assert_eq!(got, (0..24).map(|i| i * 10).collect::<Vec<_>>());
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak > 1, "the groups ran serially (peak {peak})");
        assert!(
            peak <= 6,
            "more requests in flight than connections (peak {peak})"
        );
    }

    #[test]
    fn fan_out_returns_an_error() {
        let r = for_each_bounded(8, 3, |i| if i == 5 { Err(Error::NoDaemon) } else { Ok(i) });
        assert!(r.is_err());
    }
}
