use crate::local;
use crate::relative_from::relative_from;
use crate::subtool::dynamic_task;
use anyhow::{anyhow, Context, Error, Result};
use deps_infer::c_include_parser;
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::{Derivation, DerivationOutput};
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_derivation::placeholder::Placeholder;
use harmonia_store_path::{StoreDir, StorePath};
use n2::{
    canon,
    graph::{self, Build, BuildDependencies, BuildId, File, FileId},
};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::DerivedFile;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Condvar, Mutex},
};
use walkdir::WalkDir;
use which::which;

#[derive(Clone)]
pub struct Tools {
    pub bash: StorePath,
    /// `cc` IS RESOLVED LAZILY, and the Option is the whole fix. Resolution
    /// used to be eager and fallible in `new()`, so a package with ZERO
    /// compile targets - hicolor-icon-theme is the one that bit us - died on
    /// `cc: command not found` before doing any work, having asked for a tool
    /// it was never going to use.
    /// Emission is unchanged wherever `cc` resolves: the same store path is
    /// inserted and the same `$PATH` is written, so this costs no re-key.
    pub cc: Option<StorePath>,
    pub coreutils: StorePath,
    pub nix: StorePath,
    pub nix_ninja: StorePath,
    pub nix_ninja_task: StorePath,
    pub patchelf: StorePath,
}

impl Tools {
    pub fn new(store_dir: &StoreDir) -> Result<Self> {
        Ok(Tools {
            bash: which_store_path(store_dir, "bash")?,
            cc: which_store_path(store_dir, "cc").ok(),
            coreutils: which_store_path(store_dir, "coreutils")?,
            nix: which_store_path(store_dir, "nix")?,
            nix_ninja: which_store_path(store_dir, "nix-ninja")?,
            nix_ninja_task: which_store_path(store_dir, "nix-ninja-task")?,
            patchelf: which_store_path(store_dir, "patchelf")?,
        })
    }

    /// The compiler, for the paths that genuinely need one. A build with no
    /// compile edges never reaches here, which is the point.
    pub fn require_cc(&self) -> Result<&StorePath> {
        self.cc.as_ref().ok_or_else(|| {
            anyhow!(
                "`cc` is not on PATH, and this build has a task that needs a \
                 compiler. A build with no compile targets does not reach \
                 this point."
            )
        })
    }
}

/// Task represents a fully evaluated Ninja build target.
///
/// A task contains all the context to generate a Nix derivation for the build
/// target.
#[derive(Clone)]
struct Task {
    system: String,
    wrapper_vars: HashMap<String, String>,
    input_srcs: Vec<StorePath>,

    build_dir: PathBuf,
    build_deps: BuildDependencies,
    store_dir: StoreDir,

    cmdline: Option<String>,
    desc: Option<String>,
    deps: Option<String>,
    // Ninja rspfile: (path, content) the runner must write before the
    // command runs. GN emits one per action rule; a task without it gets
    // an empty argument where the command expects the file.
    rspfile: Option<(PathBuf, String)>,
    // Ninja `depfile`: the build-dir-relative path the command writes its
    // Makefile-syntax dependency list to. Upstream #17 asks for this to
    // become an additional content-addressed output so the driver can read
    // real dependencies instead of inferring them.
    depfile: Option<String>,

    files: HashMap<FileId, File>,
    inputs: Vec<DerivedFile>,
    outputs: Vec<PathBuf>,
    // Configure-time relative symlinks to recreate in the sandbox; see
    // Runner::alias_symlinks.
    alias_symlinks: Vec<(String, String)>,
}

impl Deref for Task {
    type Target = BuildDependencies;

    fn deref(&self) -> &Self::Target {
        &self.build_deps
    }
}

/// BuildResult is the output of a Task.
pub struct BuildResult {
    pub bid: BuildId,
    pub derived_path: Option<SingleDerivedPath>,
    pub derived_files: Vec<DerivedFile>,
    pub err: Option<Error>,
}

#[derive(Clone)]
pub struct RunnerConfig {
    pub system: String,
    pub build_dir: PathBuf,
    pub store_dir: StoreDir,
    pub is_output_derivation: bool,
    /// Maximum tasks in flight. The `-j` flag parsed this and nothing
    /// consumed it; the runner spawned one thread per ready task,
    /// unbounded. On a graph with codegen fan-out (Chromium: one TU's
    /// order-only deps expand to hundreds of generated-header tasks)
    /// that produced a load average of 553 on a 24-thread machine.
    pub jobs: usize,
    /// Ninja's own per-edge concurrency classes, `name -> depth`, straight
    /// from the `pool` statements in the build files. Depth 0 is unbounded.
    ///
    /// This is the mechanism ninja provides for exactly the problem the
    /// weighting above approximates, and the graph declares it: qtwebengine's
    /// build files assign 11,114 edges to `link_pool` (depth 10) and
    /// `action_pool` (depth 24). The parser already produced both and the
    /// runner discarded them, so the driver was guessing at a limit its input
    /// stated outright.
    ///
    /// The depths are NOT independent wisdom - GN computes them on the
    /// generating machine from its CPU count and its cgroup `memory.max`, so
    /// on this host they are partly a reflection of our own ceiling. They are
    /// still the right thing to honour: they are per-EDGE, which the driver
    /// cannot derive, and being wrong the same way ninja would be wrong is a
    /// better failure than being wrong in a way nobody can reproduce.
    pub pools: HashMap<String, usize>,
    /// Ninja's `-l`. 0.0 disables, matching ninja.
    pub load_limit: f64,
}

// Counting semaphore bounding concurrent tasks. Permits release on
// Drop, so a panicking task thread cannot leak a slot.

/// Build one semaphore per bounded ninja pool.
///
/// Depth 0 is ninja's UNBOUNDED, not "admit nothing", and the two are one
/// character apart in every implementation of this: a pool built at depth 0
/// would block its edges forever, which presents as a hung build rather than
/// as a wrong limit. Pools at depth 0 get no semaphore at all.
fn pool_permits_from_depths(depths: &HashMap<String, usize>) -> HashMap<String, JobPermits> {
    depths
        .iter()
        .filter(|(_, d)| **d > 0)
        .map(|(name, depth)| (name.clone(), JobPermits::new(*depth)))
        .collect()
}

/// Inputs per unit of admission weight.
///
/// A task's memory cost tracks the input set it materializes, and that set
/// spans three orders of magnitude in one graph: a leaf TU declares tens of
/// inputs, a deep one realised 6,134 in a single call at 47 s. Counting both
/// as "one job" is what makes a single -j wrong everywhere - high enough for
/// the leaves thrashes on the deep tasks, low enough for the deep ones idles
/// 22 cores on the leaves. Measured 2026-08-20: -j3 put the machine at PSI
/// full avg10 52.78 with 29.5 GiB swapped, and -j2 left cores idle through
/// the shallow strata.
const INPUTS_PER_WEIGHT: usize = 512;

/// Memory headroom is CONTINUOUS, so the response to it is too.
///
/// The first version was a floor with a hardcoded 6 GiB: above it the full
/// budget, below it serial. That is a step function over a smooth resource,
/// and it makes the machine oscillate - admit wide, overshoot, collapse to 1,
/// recover, admit wide again. It also invented a constant, which is the same
/// defect as inventing a `-j`, one level down.
///
/// Both bounds are now derived from the machine. The reserve is MemTotal/5,
/// which on this 30.45 GiB box is 6.1 GiB and is the desktop's working set
/// plus slack; the point at which the full budget is allowed is MemTotal/2.
/// Between them the budget scales linearly, so pressure produces a taper
/// rather than a cliff.
fn memory_bounds_gib() -> (u64, u64) {
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| kb / (1024 * 1024))
        .unwrap_or(0);
    (total / 5, total / 2)
}

/// One-minute load average, or 0.0 where /proc cannot be read.
fn load_average() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

/// Admission budget under ninja's `-l`: stop starting work while the load
/// average exceeds the limit.
///
/// This is ninja's own contract and nix-ninja previously warned that it
/// accepted the flag and ignored it, which is worse than not taking it.
///
/// It is deliberately the WEAKER of the two controls here, and the reason is
/// measured rather than assumed: load average counts D-state, and a build
/// driven through the nix daemon parks many processes there waiting on the
/// store. This machine has read a load of 20.6 with PSI cpu full at 0.00 -
/// nothing was CPU-starved and the number said otherwise. So `-l` is honoured
/// because a user who passes it means it, while the memory taper above is
/// what actually protects the machine. Never reach for load when the question
/// is memory.
fn budget_for_load(cap: usize, load: f64, limit: f64) -> usize {
    if limit <= 0.0 || load < limit {
        return cap.max(1);
    }
    1
}

/// Admission budget for the memory currently available.
///
/// Pure so the curve is testable: the failures that matter are a 0 (admits
/// nothing, hangs the round) and a value above `cap` (admits more than asked
/// for), and both present only as a stall or a thrash.
fn budget_for_memory(cap: usize, avail_gib: u64, reserve_gib: u64, full_gib: u64) -> usize {
    if cap <= 1 || full_gib <= reserve_gib {
        return cap.max(1);
    }
    if avail_gib >= full_gib {
        return cap;
    }
    if avail_gib <= reserve_gib {
        return 1;
    }
    let span = full_gib - reserve_gib;
    let over = avail_gib - reserve_gib;
    let scaled = 1 + ((cap - 1) as u64 * over / span) as usize;
    scaled.clamp(1, cap)
}

/// Weight for a task declaring `inputs` inputs, clamped to `budget`.
///
/// Pure so the curve can be tested without a scheduler: the failure that
/// matters is a weight of 0 (admits unboundedly) or a weight above the budget
/// (never admits, hanging the round), and neither shows up as anything but a
/// stall.
fn admission_weight(inputs: usize, budget: usize) -> usize {
    (1 + inputs / INPUTS_PER_WEIGHT).clamp(1, budget.max(1))
}

/// Available memory in GiB, or `u64::MAX` where /proc cannot be read - an
/// unreadable meminfo must not silently serialize the whole round.
fn available_gib() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemAvailable:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| kb / (1024 * 1024))
        .unwrap_or(u64::MAX)
}

struct JobPermits {
    inner: Arc<(Mutex<usize>, Condvar)>,
    cap: usize,
    /// Ninja's `-l`: 0.0 disables, matching ninja.
    load_limit: f64,
    /// Whether admission tracks live memory. A field rather than a constant
    /// read inline so tests can disable it: with it live, a concurrency test
    /// asserts on the machine's memory at that instant and fails on a busy
    /// box for a reason unrelated to the semaphore. A flaky test about
    /// backpressure is worse than none, because it gets muted.
    memory_aware: bool,
}

struct JobPermit {
    inner: Arc<(Mutex<usize>, Condvar)>,
    weight: usize,
}

impl JobPermits {
    fn new(cap: usize) -> Self {
        JobPermits {
            inner: Arc::new((Mutex::new(0), Condvar::new())),
            cap,
            load_limit: 0.0,
            memory_aware: true,
        }
    }

    /// Ninja's `-l`. 0.0 leaves it disabled.
    fn with_load_limit(mut self, limit: f64) -> Self {
        self.load_limit = limit;
        self
    }

    /// Same, with the memory gate disabled. Tests only.
    #[cfg(test)]
    fn new_without_memory_gate(cap: usize) -> Self {
        JobPermits {
            load_limit: 0.0,
            memory_aware: false,
            ..JobPermits::new(cap)
        }
    }

    /// Blocks until `weight` units free. Blocking in the scheduler's start()
    /// is deliberate backpressure: running task threads complete and
    /// release without needing the main loop, so this cannot deadlock.
    ///
    /// The effective budget shrinks under memory pressure, which is what lets
    /// one round run the shallow strata wide and the deep ones nearly serial
    /// without anybody choosing a number in advance. A weight already clamped
    /// to the FULL budget can still exceed the shrunken one, so the wait
    /// admits it once nothing else holds a unit - otherwise low memory would
    /// hang the round rather than slow it.
    fn acquire_weighted(&self, weight: usize) -> JobPermit {
        let weight = weight.clamp(1, self.cap.max(1));
        let (lock, cvar) = &*self.inner;
        let mut count = lock.lock().unwrap();
        loop {
            let budget =
                if self.memory_aware {
                    let (reserve, full) = memory_bounds_gib();
                    // The tighter of the two controls wins. Memory is the one
                    // that matters here; load is honoured because -l was asked
                    // for, and its weakness is documented at budget_for_load.
                    budget_for_memory(self.cap, available_gib(), reserve, full)
                        .min(budget_for_load(self.cap, load_average(), self.load_limit))
                } else {
                    self.cap
                };
            if *count == 0 || *count + weight <= budget {
                break;
            }
            let (c, _) = cvar
                .wait_timeout(count, std::time::Duration::from_secs(2))
                .unwrap();
            count = c;
        }
        *count += weight;
        JobPermit {
            inner: self.inner.clone(),
            weight,
        }
    }
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.inner;
        let mut count = lock.lock().unwrap();
        *count = count.saturating_sub(self.weight);
        cvar.notify_all();
    }
}

/// Runner is an async runtime that spawns threads for each task.
pub struct Runner {
    pub derived_files: HashMap<FileId, DerivedFile>,
    build_dir_inputs: HashMap<FileId, DerivedFile>,
    // A phony build produces no artifact: it is an alias for its inputs.
    // CMake emits one per target (`cmake_object_order_depends_target_X:
    // phony || .`) as an order-only input of every TU, so without this
    // map every CMake-generated graph is unbuildable. Maps each phony
    // output FileId to the phony's input FileIds; dependents expand these
    // (transitively) in new_task instead of demanding a derivation.
    phony_aliases: HashMap<FileId, Vec<FileId>>,
    // Stamp-edge outputs -> that edge's own inputs. See the stamp comment
    // in new_task's expansion loop. Detection is by the one stamp script
    // measured in the wild (perfetto's touch_file.py); widen when a second
    // convention shows up.
    stamp_inputs: HashMap<FileId, Vec<FileId>>,
    /// Stamp-edge outputs -> the producing TASK'S fully-resolved input
    /// set, cmdline-discovered uploads included. The fid map above only
    /// carries edge-declared inputs; generate_grd names its static
    /// files (sadtab.svg) in cmdline manifests that appear on no edge,
    /// so the consumer needs what the producer actually resolved.
    stamp_input_files: HashMap<FileId, Vec<DerivedFile>>,
    /// Every output of a multi-output task, keyed by each of its outputs:
    /// a consumer depending on ONE of them gets the co-outputs too, since
    /// tools follow a declared output to files written beside it.
    co_outputs: HashMap<FileId, Vec<FileId>>,
    /// Edge outputs -> that edge's lib-shaped dependency fids: relative
    /// `.so*` files and meson's SHSYM `.symbols` indirection. A task that
    /// EXECUTES a build-produced binary (meson custom commands running a
    /// just-built tool) needs the binary's runtime libraries, and the
    /// graph hides them: the tool's link edge depends on the lib's
    /// `.symbols` file, never the lib. Expanding this map in the worklist
    /// follows .symbols -> SHSYM edge -> real lib -> its own lib deps
    /// transitively. Measured: orc's orcc dies with `liborc-0.4.so.0:
    /// cannot open shared object file` in every generator task.
    runtime_lib_deps: HashMap<FileId, Vec<FileId>>,
    /// Configure-time alias symlinks: (build-dir-relative link path,
    /// link target text), both relative, target confined to the build
    /// dir. Meson creates shared-library SONAME aliases
    /// (`liborc-0.4.so.0 -> liborc-0.4.so.0.42.0`) with os.symlink AT
    /// CONFIGURE TIME - no ninja edge produces them, the only graph
    /// trace is the `meson-implicit-outs` phony that lists them as
    /// INPUTS. runtime_lib_deps delivers the real library into a task
    /// that executes a just-built tool, and the loader then dies anyway:
    /// DT_NEEDED names the SONAME, which exists only as the alias.
    /// read_build_dir cannot upload them as opaque files either - at
    /// scan time the target may not be built yet, so canonicalize fails -
    /// and a store-level symlink object would dangle, because task
    /// materialization symlinks build paths AT the store object, making
    /// a relative target resolve against /nix/store. So the link TEXT
    /// rides NIX_NINJA_ALIASES and nix-ninja-task recreates the symlink
    /// after input setup, where the relative target resolves against
    /// whatever the task actually materialized (orc, thirteenth class,
    /// 2026-08-23).
    alias_symlinks: Vec<(String, String)>,

    tx: mpsc::Sender<BuildResult>,
    rx: mpsc::Receiver<BuildResult>,
    tools: Tools,
    rpc_client: Arc<BuilderRpcClient>,
    config: RunnerConfig,
    wrapper_vars: HashMap<String, String>,
    wrapper_store_paths: Vec<StorePath>,
    store_regex: Regex,
    permits: JobPermits,
    /// One semaphore per declared ninja pool, by name. Absent name or depth 0
    /// means unbounded, matching ninja.
    pool_permits: HashMap<String, JobPermits>,
}

impl Runner {
    pub fn new(
        tools: Tools,
        rpc_client: Arc<BuilderRpcClient>,
        config: RunnerConfig,
    ) -> Result<Self> {
        let store_dir_str = config.store_dir.to_string();
        let pattern = format!(
            r"{}\/[a-z0-9]{{32}}-[0-9a-zA-Z\+\-\._\?=]+",
            regex::escape(&store_dir_str)
        );
        let store_regex = Regex::new(&pattern)?;

        // Load the cross-round resolve cache before any task resolves,
        // and seed the client's NAR stamp cache from the previous run so
        // a restart's source-file adds become stat calls (upstream #18's
        // restart half; entries validate per hit by size+mtime).
        crate::resolve_cache::init(config.store_dir.clone(), config.build_dir.clone());
        rpc_client.seed_nar_stamps(crate::resolve_cache::load_nar_stamps());

        let mut wrapper_vars = HashMap::new();
        for (key, value) in env::vars() {
            // NIX_HARDENING_ENABLE TOO, OR A TASK COMPILES WITH NO HARDENING
            // AT ALL. cc-wrapper's add-hardening.sh reads the target-suffixed
            // NIX_HARDENING_ENABLE_<triple>, which the stdenv setup hook
            // exports into the outer build and which no task sandbox has;
            // an empty map enables nothing. Measured 2026-08-23 on alsa-lib:
            // same compiler, .text 637 kB through the drop-in against 702 kB
            // stock, and every earlier drop-in build carried the same gap.
            if key.starts_with("NIX_CFLAGS_COMPILE")
                || key.starts_with("NIX_LDFLAGS")
                || key.starts_with("NIX_CC_WRAPPER")
                || key.starts_with("NIX_BINTOOLS_WRAPPER")
                || key.starts_with("NIX_HARDENING_ENABLE")
            {
                wrapper_vars.insert(key, value);
            }
        }

        // Remove -frandom-seed from NIX_CFLAGS_COMPILE* as we'll calculate it
        // per task derivation. Otherwise this will be different every time
        // breaking incrementality.
        for (key, value) in wrapper_vars.iter_mut() {
            if key.starts_with("NIX_CFLAGS_COMPILE") {
                *value = remove_frandom_seed(value);
            }
            // SAME CLASS, OTHER VARIABLE. In a drop-in the outer
            // derivation's cc-wrapper puts `-rpath $out/lib` into
            // NIX_LDFLAGS, and $out is the OUTER output path: it moves with
            // every change to the outer derivation, a source edit included,
            // so every task re-keyed on it and a one-line edit rebuilt all
            // 105 of qtsvg's TUs (measured 2026-08-23, after the input
            // blanket was already off for compiles - this was the last
            // term). Nothing inside a task needs it: a compile never links,
            // and the install step rewrites RPATH off the build tree anyway.
            if key.starts_with("NIX_LDFLAGS") {
                let before = value.clone();
                *value = remove_outer_rpath(value);
                if std::env::var_os("NIX_NINJA_DIAG").is_some() {
                    eprintln!(
                        "nix-ninja: DIAG {key} outer={:?} before=[{}] after=[{}]",
                        outer_output_paths(),
                        &before[..before.len().min(120)],
                        &value[..value.len().min(120)]
                    );
                }
            }
        }

        // Outer output paths -> placeholders in the forwarded env too.
        let rewrite = outer_rewrite_map();
        for value in wrapper_vars.values_mut() {
            *value = rewrite_str(value, &rewrite);
        }

        // Extract store paths from wrapper variables once
        let mut wrapper_store_paths = Vec::new();
        for value in wrapper_vars.values() {
            let found_store_paths = extract_store_paths(&config.store_dir, &store_regex, value)?;
            wrapper_store_paths.extend(found_store_paths);
        }

        let (tx, rx) = mpsc::channel();
        let permits = JobPermits::new(config.jobs.max(1)).with_load_limit(config.load_limit);
        let pool_permits = pool_permits_from_depths(&config.pools);
        Ok(Runner {
            derived_files: HashMap::new(),
            build_dir_inputs: HashMap::new(),
            alias_symlinks: Vec::new(),
            phony_aliases: HashMap::new(),
            stamp_inputs: HashMap::new(),
            stamp_input_files: HashMap::new(),
            co_outputs: HashMap::new(),
            runtime_lib_deps: HashMap::new(),
            permits,
            pool_permits,
            tx,
            rx,
            tools,
            rpc_client,
            config,
            wrapper_vars,
            wrapper_store_paths,
            store_regex,
        })
    }

    // Build systems like Meson may generate files via `configure_file` that are
    // not listed as implicit inputs in the build.ninja file. So we must read
    // the build directory and consider them implict inputs for all tasks.
    pub fn read_build_dir(&mut self, files: &mut graph::GraphFiles) -> Result<()> {
        for entry in WalkDir::new(&self.config.build_dir)
            .into_iter()
            .filter_entry(|e| {
                // Skip directories that start with "meson-" as they contain
                // non-deterministic internal data from meson
                !e.file_name().to_string_lossy().starts_with("meson-")
            })
        {
            // A file that vanishes between listing and reading is a build
            // in progress, not an error: skip it rather than failing the
            // whole driver on a temp file (libtool's `.loT`, alsa-lib).
            let entry = match entry {
                Ok(e) => e,
                Err(e)
                    if e.io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(e) => return Err(e.into()),
            };
            if entry.file_type().is_symlink() {
                let path = entry.into_path();
                if let Some(pair) = alias_symlink_entry(&self.config.build_dir, &path) {
                    self.alias_symlinks.push(pair);
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            let derived_file =
                match new_opaque_file(&self.rpc_client, &self.config.build_dir, path.clone()) {
                    Ok(d) => d,
                    Err(e)
                        if e.downcast_ref::<std::io::Error>()
                            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        continue
                    }
                    Err(e) => return Err(e),
                };
            let fid = self.add_derived_file(files, derived_file.clone());
            self.build_dir_inputs.insert(fid, derived_file);
        }
        // Deterministic order: the encoded env value must be a function
        // of the SET, not of readdir order, or every run re-keys every
        // task derivation.
        self.alias_symlinks.sort();
        self.alias_symlinks.dedup();
        Ok(())
    }

    pub fn start(
        &mut self,
        files: &mut graph::GraphFiles,
        bid: BuildId,
        build: &Build,
    ) -> Result<()> {
        let tx = self.tx.clone();

        // A phony rule has no command: record the alias and complete the
        // build immediately, spawning nothing. Ordering falls out of the
        // nix model for free - dependents inherit the phony's inputs as
        // their own derivation inputs via the expansion in new_task.
        if build.cmdline.is_none() {
            let ins: Vec<FileId> = build.ordering_ins().to_vec();
            for fid in build.outs() {
                self.phony_aliases.insert(*fid, ins.clone());
            }
            tx.send(BuildResult {
                bid,
                derived_path: None,
                derived_files: Vec::new(),
                err: None,
            })
            .context("completing phony build")?;
            return Ok(());
        }

        // Record stamp edges BEFORE building the task, so any later
        // consumer's expansion sees them (ninja emits producers before
        // consumers in the traversal order the driver walks).
        // ts_library.py edges have the same shape as stamps from a
        // consumer's viewpoint: the generated tsconfig lists its
        // project's SOURCE .d.ts files (a definitions target compiles
        // nothing - tsc passes .d.ts through), so following a project
        // reference needs the producing edge's INPUTS beside the
        // tsconfig. Same recording, same expansion; the via_phony
        // not-a-file drop discards entries that are another task's
        // unmaterialized outputs.
        if build.cmdline.as_deref().is_some_and(|c| {
            c.contains("touch_file.py")
                    || c.contains("ts_library.py")
                    // generate_grd emits a manifest whose consumer (grit)
                    // reads the files the producer's PHONY inputs carry
                    // (preprocess_static_files: the preprocessed css);
                    // recording its edge inputs lets the worklist's phony
                    // expansion materialize them for the consumer.
                    || c.contains("generate_grd.py")
        }) {
            let ins: Vec<FileId> = build.dirtying_ins().to_vec();
            for fid in build.outs() {
                self.stamp_inputs.insert(*fid, ins.clone());
            }
        }

        // Record co-outputs BEFORE building the task, same ordering
        // argument as the stamp edges above.
        if build.outs().len() > 1 {
            let outs: Vec<FileId> = build.outs().to_vec();
            for fid in build.outs() {
                self.co_outputs.insert(*fid, outs.clone());
            }
        }

        // Record lib-shaped deps BEFORE building the task, same ordering
        // argument again. Shape: a RELATIVE path (a store-path .so is
        // already visible in every sandbox) whose basename carries `.so`
        // as an extension boundary, or meson's `.symbols` relink guard,
        // which is the only edge-visible handle on the lib it certifies.
        let libs: Vec<FileId> = build
            .dirtying_ins()
            .iter()
            .filter(|f| lib_shaped(&files.by_id[**f].name))
            .copied()
            .collect();
        if !libs.is_empty() {
            for fid in build.outs() {
                self.runtime_lib_deps.insert(*fid, libs.clone());
            }
        }

        let tools = self.tools.clone();
        // Main-loop heartbeat: resolution runs serially here, so when it
        // degrades the process shows one pegged thread, a silent log and
        // an idle daemon - indistinguishable from a hang from outside
        // (measured, round 58: 22 min without a daemon connection). The
        // counter names the slow task and prices resolution per task; it
        // is always on because its cost is two atomics per task.
        use std::sync::atomic::{AtomicU64, Ordering};
        static TASKS: AtomicU64 = AtomicU64::new(0);
        static RESOLVE_MS: AtomicU64 = AtomicU64::new(0);
        let t0 = std::time::Instant::now();
        let task = self.new_task(files, build)?;
        let resolve_ms = t0.elapsed().as_millis() as u64;
        RESOLVE_MS.fetch_add(resolve_ms, Ordering::Relaxed);
        let n_tasks = TASKS.fetch_add(1, Ordering::Relaxed) + 1;
        if resolve_ms > 5_000 {
            eprintln!(
                "nix-ninja: SLOW RESOLVE {resolve_ms} ms for {} (task {n_tasks})",
                build
                    .outs()
                    .first()
                    .map(|f| files.by_id[*f].name.as_str())
                    .unwrap_or("?")
            );
        }
        if n_tasks.is_multiple_of(500) {
            // Persist resolve-cache entries computed since the last tick;
            // a killed driver loses at most one tick's worth.
            if let Err(e) = crate::resolve_cache::flush() {
                eprintln!("nix-ninja: resolve cache flush failed: {e}");
            }
            if let Err(e) =
                crate::resolve_cache::save_nar_stamps(&self.rpc_client.nar_stamps_snapshot())
            {
                eprintln!("nix-ninja: nar stamp save failed: {e}");
            }
            // Each stats() call is a fresh pair of atomic loads, so calling
            // one twice in an argument list samples two different instants
            // while other threads advance the counters - which can print a
            // sent count above its own total, or parsed above reached. Read
            // each family ONCE into a tuple. Reported by the specification
            // session against scan_stats; realise and nar had it too, and
            // that is why all three are read here rather than the one that
            // was noticed.
            // realise_stats is (ASKED, SENT) - the first element is ALREADY
            // the total, unlike the other two, which are (hits, count) and
            // need summing. The original argument list encoded that
            // difference positionally and it is easy to lose in a rename.
            let (rl_asked, rl_sent) = nix_builder_rpc_client::realise_stats();
            let (nar_hits, nar_sent) = nix_builder_rpc_client::nar_upload_stats();
            // parsed / reached: misses are files actually read, the sum is
            // every time a TU needed one. The gap is the sharing.
            let (scan_hit, scan_miss) = deps_infer::c_include_parser::scan_stats();
            // Same tear as the three families above, and it bites HARDER here
            // because the pair is read as a RATIO: a ms total from one instant
            // over a call count from another prints a per-call cost that was
            // never true. Written separately once in this same session, an hour
            // after fixing the identical defect - the class does not announce
            // itself on the way back in.
            //
            // AND THIS IS A TUPLE SYNTACTICALLY, NOT AN ATOMIC READ. The two
            // loads are still two instants; what the binding buys is a window
            // of nanoseconds instead of one spanning a whole format call, which
            // is enough for a figure a person reads and is NOT enough for one
            // code consumes. If a per-call number ever feeds a threshold or a
            // regression gate, pack ms and n into a single AtomicU64 (32/32
            // covers these magnitudes) or take a Mutex on the slow path. Said
            // here because "read as a pair" reads like atomicity and is not.
            // Raised by the specification session, addendum 734.
            let (upd_ms, upd_n) = (
                DYN_UPDATE_MS.load(Ordering::Relaxed),
                DYN_UPDATE_N.load(Ordering::Relaxed),
            );
            let (add_ms, add_n) = (
                DYN_ADDDRV_MS.load(Ordering::Relaxed),
                DYN_ADDDRV_N.load(Ordering::Relaxed),
            );
            // Appended rather than given a fixed `{}` in the format string,
            // so a platform without mallinfo2 prints a shorter line instead
            // of two zeros that read as a measurement.
            let (declared, kept) = (
                DECLARED_HEADERS.load(Ordering::Relaxed),
                USED_HEADERS.load(Ordering::Relaxed),
            );
            // Printed as a pair with its denominator: "N prunable" alone is
            // unreadable without knowing how many were considered, and this
            // number exists to be compared against the cost of acting on it.
            let prune = prune_line(declared, kept);
            let heap = match self_heap_mib() {
                Some((live, retained)) => {
                    format!(", heap {live} MiB live / {retained} MiB retained")
                }
                None => String::new(),
            };
            eprintln!(
                "nix-ninja: resolved {n_tasks} tasks, {} s total resolve time \
                 (worklist {} s, cmdline {} s, py {} s, grd {} s), \
                 dyn {} s (realise {} s, discover {} s, update {} s/{} calls, adddrv {} s/{} calls, \
                 plain adddrv {} s/{} calls), \
                 realise {}/{} sent, nar {}/{} sent, scan {}/{} parsed, \
                 rss {} MiB{}{}",
                RESOLVE_MS.load(Ordering::Relaxed) / 1000,
                NT_WORKLIST_MS.load(Ordering::Relaxed) / 1000,
                NT_CMDLINE_MS.load(Ordering::Relaxed) / 1000,
                NT_PY_MS.load(Ordering::Relaxed) / 1000,
                NT_GRD_MS.load(Ordering::Relaxed) / 1000,
                DYN_MS.load(Ordering::Relaxed) / 1000,
                DYN_REALISE_MS.load(Ordering::Relaxed) / 1000,
                DYN_DISCOVER_MS.load(Ordering::Relaxed) / 1000,
                upd_ms / 1000,
                upd_n,
                add_ms / 1000,
                add_n,
                DYN_PLAIN_ADDDRV_MS.load(Ordering::Relaxed) / 1000,
                DYN_PLAIN_ADDDRV_N.load(Ordering::Relaxed),
                rl_sent,
                rl_asked,
                nar_sent,
                nar_hits + nar_sent,
                scan_miss,
                scan_hit + scan_miss,
                self_rss_mib(),
                heap,
                prune,
            );
        }

        // Stamp-tool edges also record the task's RESOLVED inputs (the
        // fid map above records only edge-declared ones): generate_grd
        // names its static files in cmdline manifests that appear on no
        // edge, and the grit consumer needs them.
        if build.cmdline.as_deref().is_some_and(|c| {
            c.contains("touch_file.py")
                || c.contains("ts_library.py")
                || c.contains("generate_grd.py")
        }) {
            let mut record = task.inputs.clone();
            // generate_grd's SOURCE files exist only on its cmdline, as
            // bare names relative to --input-files-base-dir under the
            // repo root (never as edge inputs - the tool only writes a
            // manifest naming them). grit reads their CONTENT two edges
            // downstream (grd -> grdp -> file), so resolve and upload
            // them into the record here; the consumer's worklist walks
            // grd -> stamp_inputs -> grdp fids -> this record.
            if build
                .cmdline
                .as_deref()
                .is_some_and(|c| c.contains("generate_grd.py"))
            {
                for p in generate_grd_input_files(build.cmdline.as_deref().unwrap()) {
                    let up = new_opaque_file(&self.rpc_client, &self.config.build_dir, p)?;
                    record.push(up);
                }
            }
            for fid in build.outs() {
                self.stamp_input_files.insert(*fid, record.clone());
            }
        }

        // Acquire before spawning: bounds thread count AND daemon load.
        // Phony builds returned above and never consume a slot. The
        // permit moves into the thread and releases on drop, panic-safe.
        //
        // Weighted by the task's declared input count, which is the best
        // predictor of its memory cost available at admission time and was
        // previously thrown away by counting every task as one. Small tasks
        // run wide, deep ones approach serial, and the same round does both.
        let permit = self
            .permits
            .acquire_weighted(admission_weight(task.inputs.len(), self.permits.cap));

        // THEN the edge's declared ninja pool, if it has one. Order matters
        // and is always global-then-pool: a thread waiting on a pool already
        // holds its global units, so the threads that will free the pool are
        // never blocked behind it and the pair cannot deadlock. Reversing it
        // would let pool holders queue for global slots held by pool waiters.
        let pool_permit = build
            .pool
            .as_ref()
            .and_then(|name| self.pool_permits.get(name))
            .map(|p| p.acquire_weighted(1));

        let config = self.config.clone();
        let rpc_client = self.rpc_client.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let _pool_permit = pool_permit;
            let (derived_path, err) = match build_task_derivation(
                tools.clone(),
                &rpc_client,
                task.clone(),
            ) {
                Ok(drv) => match handle_derivation_result(
                    tools.clone(),
                    &rpc_client,
                    task.clone(),
                    drv.clone(),
                    &config,
                ) {
                    Ok(final_derived_path) => (Some(final_derived_path), None),
                    Err(err) => {
                        // Cause FIRST and no multi-megabyte JSON dump:
                        // at Chromium scale the dump swallowed the cause
                        // chain and cost blind debugging rounds.
                        (None, Some(err.context(format!(
                                "Failed to handle derivation result for task (derivation: {}, {} inputs)",
                                drv.name,
                                task.inputs.len()
                            ))))
                    }
                },
                Err(err) => (
                    None,
                    Some(err.context("Failed to build task derivation for task".to_string())),
                ),
            };

            // Create DerivedFiles for all outputs if successful
            let derived_files = if let Some(ref final_derived_path) = derived_path {
                let mut drv_outputs: Vec<DerivedFile> = Vec::new();
                for fid in task.outs() {
                    let file = &task.files[fid];
                    // Normalization already happened in new_task for
                    // task.outputs; apply the same here so the recorded
                    // DerivedFile agrees with what the task wrote.
                    match normalize_build_path(&config.build_dir, file.name.clone().into()) {
                        Ok(p) => drv_outputs.push(new_built_file(final_derived_path.clone(), p)),
                        Err(e) => {
                            eprintln!("Error: {e}");
                        }
                    }
                }
                // THE TREE, BY THE SAME RULE new_task USED. Its FileId is
                // synthetic - ninja has no node for a directory - so it can
                // only reach a consumer by being attached to the outputs the
                // graph DOES know, which is what the caller does below.
                let paths: Vec<PathBuf> =
                    drv_outputs.iter().map(|d| d.build_path.clone()).collect();
                for dir in undeclared_outputs(&paths, task.cmdline.as_deref(), &config.build_dir) {
                    if !paths.contains(&dir) {
                        drv_outputs.push(new_built_file(final_derived_path.clone(), dir));
                    }
                }
                drv_outputs
            } else {
                Vec::new()
            };

            let result = BuildResult {
                bid,
                derived_path,
                derived_files,
                err,
            };
            let _ = tx.send(result);
        });

        Ok(())
    }

    /// Returns the finished build and whether it SUCCEEDED. A failure is
    /// fully reported here (error chain to stderr); the scheduler decides
    /// whether it is fatal - under keep-going it abandons the failed
    /// subtree and drains the rest, so one round surfaces every
    /// independent failure instead of the first.
    pub fn wait(&mut self, files: &mut graph::GraphFiles) -> Result<(BuildId, bool)> {
        let result = self.rx.recv().unwrap();
        if let Some(err) = result.err {
            eprintln!("Error: {err}");

            eprintln!("Caused by:");
            for cause in err.chain().skip(1) {
                eprintln!("    {cause}");
            }

            let debug_info = if let Some(derived_path) = &result.derived_path {
                format!(
                    "derivation: {}",
                    self.config.store_dir.display(derived_path)
                )
            } else {
                format!("build_id: {:?}", result.bid)
            };
            eprintln!("Failed to build task derivation for {}", debug_info);

            return Ok((result.bid, false));
        }

        // A DIRECTORY OUTPUT REACHES CONSUMERS ONLY BY RIDING A REAL ONE.
        // co_outputs is keyed by ninja FileIds and lists a rule's declared
        // outs; a directory is not a graph node, so nothing would ever
        // expand to it. Attaching its synthetic id to the co-output list of
        // each real output of the same rule puts it exactly where the public
        // forwarding headers already travel - which is how they reach a
        // translation unit's input list today.
        let mut fids: Vec<FileId> = Vec::new();
        let mut tree_fids: Vec<FileId> = Vec::new();
        for derived_file in result.derived_files {
            let is_tree = is_tree_path(&derived_file.build_path);
            let fid = self.add_derived_file(files, derived_file.clone());
            if is_tree {
                tree_fids.push(fid);
            } else {
                fids.push(fid);
            }
        }
        for tf in &tree_fids {
            for fid in &fids {
                let entry = self.co_outputs.entry(*fid).or_insert_with(|| vec![*fid]);
                if !entry.contains(tf) {
                    entry.push(*tf);
                }
            }
        }

        Ok((result.bid, true))
    }

    fn add_derived_file(
        &mut self,
        files: &mut graph::GraphFiles,
        derived_file: DerivedFile,
    ) -> FileId {
        let mut path_str = derived_file.build_path.to_string_lossy().into_owned();
        // A TREE IS KEYED APART FROM THE GRAPH NODE OF THE SAME NAME. CMake
        // emits a phony target called `<T>_autogen`, the directory's own
        // name; keyed by that string the tree took the phony's FileId, and
        // consumer expansion followed phony_aliases to the timestamp and
        // never reached the tree (main.moc missing, 2026-08-23). The key
        // is only ever reached through co_outputs, so the marker changes
        // nothing but the lookup; the DerivedFile keeps its real path.
        if is_tree_path(&derived_file.build_path) {
            path_str.push_str("/.nn-tree");
        }
        let fid = match files.lookup(&path_str) {
            Some(fid) => fid,
            None => {
                // AN OUTPUT OUTSIDE THE BUILD DIR HAS TWO SPELLINGS AND THE
                // GRAPH ONLY KNOWS ONE. CMake declares such an output
                // ABSOLUTE in build.ninja (openfec sets its library dir to
                // ${SRC}/bin/Release, a SIBLING of the build dir), while
                // normalize_build_path hands the task - and therefore this
                // registration - the `../` climb. Keyed by the climb, the
                // finished output takes a fresh FileId, the graph node's
                // consumers miss it in derived_files, and input assembly
                // falls through to an opaque upload of a file that only
                // ever existed in the producer's sandbox: `canonicalize
                // ../bin/Release/libopenfec.so.1.4.2: No such file`
                // (openfec 1.4.2.12, server edition attempt 5,
                // 2026-08-23). Try the absolute spelling before minting a
                // new id; the DerivedFile keeps its relative build_path,
                // which is the one the sandbox layout is built from.
                let abs_fid = if path_str.starts_with("../") {
                    let mut abs =
                        format!("{}/{}", self.config.build_dir.to_string_lossy(), path_str);
                    canon::canonicalize_path(&mut abs);
                    files.lookup(&abs)
                } else {
                    None
                };
                match abs_fid {
                    Some(fid) => fid,
                    None => files.id_from_canonical(path_str),
                }
            }
        };

        self.derived_files.entry(fid).or_insert(derived_file);

        fid
    }

    /// Every concrete output a requested TARGET resolves to.
    ///
    /// A phony target has no derived file of its own: this fork records it in
    /// `phony_aliases` and expands it at input assembly, so dependents inherit
    /// its inputs as their own derivation inputs. That is the right model for
    /// the graph and it leaves one gap at the CLI boundary, where a phony
    /// named as a target would otherwise resolve to nothing and report a
    /// missing derived file for a build that in fact succeeded.
    ///
    /// Expansion is transitive because a phony may alias another phony, and
    /// the seen-set is what makes an alias cycle terminate rather than hang -
    /// ninja permits the file to declare one, so it is reachable input, not a
    /// defensive flourish.
    pub fn resolve_target(&self, fid: FileId) -> Vec<DerivedFile> {
        resolve_target_in(&self.derived_files, &self.phony_aliases, fid)
    }

    /// Whether a target is a phony alias. A header-only CMake project emits
    /// `build all: phony` with ZERO inputs (opencl-headers: every real edge
    /// is a utility target outside `all`), so resolving it is legitimately
    /// empty and real ninja succeeds doing nothing. The caller uses this to
    /// tell that no-op apart from a target that failed to resolve.
    pub fn is_phony(&self, fid: FileId) -> bool {
        self.phony_aliases.contains_key(&fid)
    }

    fn new_task(&mut self, files: &mut graph::GraphFiles, build: &Build) -> Result<Task> {
        // Section clocks for the serial-resolution bottleneck, read by
        // the heartbeat in start(). Wall time between checkpoints lands
        // in the named section's bucket.
        let mut sec_t = std::time::Instant::now();
        let mut lap = |bucket: &std::sync::atomic::AtomicU64| {
            let now = std::time::Instant::now();
            bucket.fetch_add(
                (now - sec_t).as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            sec_t = now;
        };
        let store_dir = self.config.store_dir.to_string();

        // Provide the task access to all the original files for explicit
        // inputs and implicit/explicit outputs.
        let mut build_files: HashMap<FileId, File> = HashMap::new();
        for fid in build.ordering_ins().iter().chain(build.outs()) {
            build_files.insert(*fid, files.by_id[*fid].clone());
        }

        // Iterate over all explict, implicit and order-only dependencies as
        // they must all be linked into the derivation's source directory.
        // FxHashMap, not the default SipHash map: perf on round 64 put
        // 30% of all driver cycles in SipHash over PathBuf keys of this
        // map (each ts_library task re-inserts a multi-thousand-entry
        // memoized closure), and FxHash is not DoS-hardened, which this
        // map does not need - its keys are the build graph's own paths.
        let mut input_set: rustc_hash::FxHashMap<PathBuf, DerivedFile> =
            rustc_hash::FxHashMap::default();
        // Expand phony aliases (transitively) into the real inputs behind
        // them. `via_phony` marks fids reached through an expansion: a
        // pure-ordering token that is not a file (CMake emits `phony || .`,
        // the build dir itself) is silently dropped there, whereas a
        // missing DIRECT input stays a loud error as before.
        // A COMPILE, WHICH IS WHAT ALL FOUR USES BELOW SAY THEY MEAN.
        // `deps = gcc` was the same set when this was written, and CMake
        // broke the equivalence: since it began generating link depfiles
        // (`-Wl,--dependency-file`), its own CMakeFiles/rules.ninja carries
        // `depfile = $DEP_FILE` and `deps = gcc` on the LINKER rules too,
        // not just the compiler ones. Read off a generated rules.ninja
        // 2026-08-26: rule C_SHARED_LIBRARY_LINKER__capstone_shared_Release
        // has both lines.
        //
        // So every link was classified as a compile and lost the four
        // things this gate withholds from compiles - most consequentially
        // the implicit-input blanket, whose own comment names the failure
        // that came back: zlib-ng's link died with `cannot open linker
        // script file .../zlib-ng.map`, a configure-generated file no edge
        // declares. The gate was added to keep TU closures lean and to stop
        // a shared-file edit re-keying every TU; both of those are about
        // compiles, and neither argument reaches a link.
        //
        // Object-shaped outputs are the discriminator because they are what
        // a compile IS, and the polarity is deliberate: anything not
        // provably a compile falls to the conservative side and keeps the
        // blanket. A link misread as a compile fails to build; a link
        // correctly excluded merely carries a larger input set, which is
        // what it did before the gate existed. `.lo` is here for libtool,
        // which drives fftw's per-TU compiles through this same path.
        let is_gcc_task = is_compile_task(
            build.deps.as_deref(),
            build
                .outs()
                .iter()
                .map(|fid| files.by_id[*fid].name.as_str()),
        );
        let mut worklist: Vec<(FileId, bool)> =
            build.ordering_ins().iter().map(|f| (*f, false)).collect();
        let mut seen: rustc_hash::FxHashSet<FileId> = rustc_hash::FxHashSet::default();
        while let Some((fid, via_phony)) = worklist.pop() {
            if !seen.insert(fid) {
                continue;
            }
            if let Some(alias_ins) = self.phony_aliases.get(&fid) {
                worklist.extend(alias_ins.iter().map(|f| (*f, true)));
                continue;
            }
            // A stamp file certifies its edge's inputs EXIST; it carries no
            // content. On a shared filesystem depending on the stamp is
            // enough, but in the task sandbox the certified files must be
            // materialized too: perfetto's trace_processor_table_generator
            // stamps python package files that gen_tp_table_headers.py then
            // imports from the repo root. So a stamp dependency also
            // enqueues the stamp edge's own inputs, marked via_phony so the
            // gcc header filter and the not-a-file drop still apply. The
            // stamp fid itself continues below as a normal task-output dep.
            if let Some(extra) = self.stamp_inputs.get(&fid) {
                worklist.extend(extra.iter().map(|f| (*f, true)));
            }
            if let Some(extra) = self.stamp_input_files.get(&fid) {
                for df in extra {
                    if is_gcc_task {
                        let n = df.build_path.to_string_lossy();
                        let header_like = n.ends_with(".h")
                            || n.ends_with(".hpp")
                            || n.ends_with(".hh")
                            || n.ends_with(".inc")
                            || n.ends_with(".ipp");
                        if !header_like {
                            continue;
                        }
                    }
                    input_set
                        .entry(df.build_path.clone())
                        .or_insert_with(|| df.clone());
                }
            }
            // An input produced by a multi-output task pulls in its
            // CO-OUTPUTS: tools follow one declared output to the files
            // written beside it (tsc reads a project-referenced
            // tsconfig.json, then the .d.ts files its producing task
            // emitted next to it - MojoHandle's definition among them;
            // upstream never declares those because a shared build dir
            // makes following them free). Marked via_phony so the gcc
            // header filter keeps TU closures lean and a co-output that
            // is not yet a file drops silently rather than erroring.
            if let Some(sibs) = self.co_outputs.get(&fid) {
                worklist.extend(sibs.iter().filter(|s| **s != fid).map(|s| (*s, true)));
            }
            // A build-produced binary this task may EXECUTE needs its
            // runtime libraries materialized; enqueueing the producing
            // edge's lib-shaped deps lets the worklist follow meson's
            // .symbols indirection to the real lib transitively. Compile
            // tasks never execute build outputs, and their closures are
            // kept lean by the same is_gcc_task gate as the phony filter.
            if !is_gcc_task {
                if let Some(libs) = self.runtime_lib_deps.get(&fid) {
                    worklist.extend(libs.iter().map(|f| (*f, true)));
                }
            }
            // For a compile (deps=gcc), a phony-EXPANDED order-only dep is
            // only a real input if it is header-shaped: expansion of GN's
            // inputdeps phonies otherwise drags generated OBJECTS and
            // SOURCES (perfetto's entire .gen.o world, measured) into one
            // TU's closure. Directly declared order-only deps (a generated
            // buildflags header on the edge itself) are never filtered.
            if via_phony && is_gcc_task {
                let name = &files.by_id[fid].name;
                // A MODULE INCLUDE TREE IS HEADERS BY CONSTRUCTION and has
                // no suffix to say so: syncqt's `include/<Module>` rides
                // its real outputs as a co-output (wait(), above), arrives
                // here via_phony, and this filter dropped it from every
                // compile task - so `cmake_pch.hxx`'s `#include
                // <QtSvg/QtSvg>`, the undeclared master header the tree
                // exists to carry, was absent from the SvgWidgets PCH
                // task while the Svg syncqt task's log listed it among
                // the tree's eight files (qtsvg, 2026-08-23). Same
                // predicate wait() uses to recognise the tree.
                let tree_like = is_tree_path(Path::new(name.as_str()));
                let header_like = name.ends_with(".h")
                    || name.ends_with(".hpp")
                    || name.ends_with(".hh")
                    || name.ends_with(".inc")
                    || name.ends_with(".ipp")
                    || tree_like;
                if !header_like {
                    continue;
                }
            }
            let input = match self.derived_files.get(&fid) {
                Some(df) => df.to_owned(),
                None => {
                    let file = &files.by_id[fid];
                    if via_phony && !Path::new(&file.name).is_file() {
                        continue;
                    }
                    if file.name.starts_with(&store_dir) {
                        // TODO: Perhaps need to add this as inputSrc? But
                        // will also have to change DerivedFile to have source
                        // Option<PathBuf>, because we don't want it to be
                        // added to $NIX_NINJA_INPUTS.
                        // DerivedFile {
                        //     path: SingleDerivedPath::Opaque(StorePath::new(file.name)),
                        //     source: &file.name,
                        // }
                        continue;
                    }

                    let uploaded = upload_referenced_file(
                        &self.rpc_client,
                        &self.config.build_dir,
                        PathBuf::from(&file.name),
                    )
                    .with_context(|| {
                        format!(
                            "declared input {} of edge producing {:?} (via_phony={})",
                            file.name,
                            build.outs().first().map(|f| files.by_id[*f].name.clone()),
                            via_phony,
                        )
                    })?;
                    for extra in &uploaded[1..] {
                        self.add_derived_file(files, extra.clone());
                        input_set.insert(extra.build_path.clone(), extra.clone());
                    }
                    let input = uploaded.into_iter().next().unwrap();
                    self.add_derived_file(files, input.clone());
                    input
                }
            };
            input_set.insert(input.build_path.clone(), input.clone());
        }

        let mut outputs: Vec<PathBuf> = Vec::new();
        for fid in build.outs() {
            let file = &files.by_id[*fid];
            // See normalize_build_path: an absolute output escapes the
            // task sandbox via Path::join's prefix-discarding semantics.
            outputs.push(normalize_build_path(
                &self.config.build_dir,
                PathBuf::from(&file.name),
            )?);
        }

        lap(&NT_WORKLIST_MS);

        // Meson resolves `files(...)` in custom_target commands to absolute
        // paths at configure time. Those paths do not exist inside the task
        // sandbox, where inputs are symlinked at their build-dir-relative
        // locations, so rewrite in-tree absolute paths to relative ones
        // (mirroring `relative_from` in `new_opaque_file`).
        // GN goes further than meson: it bakes absolute paths to ANY
        // ancestor of the build dir (qtwebengine's gen_icui18n_shim passes
        // --headers-root five levels up, and the script computes
        // relpath(root, source_tree) from it, silently writing its outputs
        // outside the declared tree when root is a host-absolute path). So
        // rewrite every ancestor prefix, deepest first - each occurrence of
        // "<ancestor>/" becomes the "../" chain that reaches it from the
        // build dir. Deepest-first matters: a deeper prefix contains every
        // shallower one. Stop above 3 components so "/home/", "/nix/" and
        // "/" are never rewritten.
        let cmdline = build
            .cmdline
            .as_ref()
            .map(|cmdline| rewrite_cmdline(cmdline, &self.config.build_dir));

        // The rebase root for arguments GN wrote relative to the target's
        // own gen dir: the first output's directory.
        let outputs_hint: Option<PathBuf> = outputs
            .first()
            .and_then(|o| o.parent())
            .map(Path::to_path_buf);

        // TODO: Can we avoid this? Technically the build rule isn't complete.
        //
        // The command may reference a file pre-generated by the configuration
        // step. We tracked files that existed in the build directory
        // beforehand, so we can see if there's anything that matches and add
        // it as an explicit input.
        let mut node_args: Vec<FileId> = Vec::new();
        // An rsp-style `@file` argument the graph never declares, plus the
        // cmdline rewrite it needs. Filled by the `@` branch below, applied
        // after the loop (the loop holds cmdline borrowed).
        let mut extra_rspfile: Option<(PathBuf, String)> = None;
        let mut arg_rewrites: Vec<(String, String)> = Vec::new();
        // Indices into the split argv that the `@` branch respelled, so the
        // same respelling can be applied to the ORIGINAL argv below.
        let mut rewritten_idx: Vec<usize> = Vec::new();
        if let Some(cmdline) = &cmdline {
            let args = shell_words::split(cmdline)?;
            // CMake custom commands open with `cd <subdir> &&`; every
            // relative path the command or an rsp file resolves after that
            // is relative to the subdir, not to the build root the rewrite
            // above targeted. Depth of the cd target = the `../` levels a
            // post-cd reference needs to climb back to the build root.
            let cd_depth = if args.len() >= 3
                && args[0] == "cd"
                && args[2] == "&&"
                && !args[1].starts_with('/')
            {
                Path::new(&args[1])
                    .components()
                    .filter(|c| matches!(c, std::path::Component::Normal(_)))
                    .count()
            } else {
                0
            };
            // json_schema_compiler resolves a cross-namespace type
            // (extensionTypes.InjectDetails from web_view_internal.json) by
            // loading the referenced namespace's schema from the same api
            // dir by NAMING CONVENTION - files no edge declares (upstream
            // relies on a shared filesystem). The round-73 rule gated on
            // .idl/.webidl and sat below the plain file-upload branch that
            // catches every existing file first, so a schema arg uploaded
            // only itself. The right discriminator is the TOOL, not the
            // extension: any schema arg to compiler.py implies its schema
            // directory.
            let schema_tool = cmdline.contains("json_schema_compiler/compiler.py");
            for (arg_i, arg) in args.into_iter().enumerate() {
                // The schema-dir rule must sit ABOVE the node dispatch: the
                // schema file is a DECLARED input on these edges, so
                // files.lookup succeeds and the else-branches below never
                // see it (round 76: all 63 failures were this rule placed
                // on the path the arg never takes).
                if schema_tool
                    && !arg.starts_with('-')
                    && !arg.starts_with('/')
                    && Path::new(&arg)
                        .extension()
                        .is_some_and(|e| e == "json" || e == "idl" || e == "webidl")
                    && Path::new(&arg).is_file()
                {
                    if let Some(dir) = Path::new(&arg).parent() {
                        for input in
                            upload_referenced_dir(&self.rpc_client, &self.config.build_dir, dir)?
                        {
                            self.add_derived_file(files, input.clone());
                            input_set.insert(input.build_path.clone(), input);
                        }
                    }
                }
                let Some(fid) = files.lookup(&arg) else {
                    // A `@file` argument is a response file the tool reads
                    // itself (qtbase's syncqt), invisible to every branch
                    // below because of the prefix. Three needs at once:
                    // the file's CONTENT carries absolute host paths
                    // (rewrite them, cwd-compensated - syncqt resolves
                    // them from the post-cd cwd); the source dirs that
                    // content names must be UPLOADED (syncqt scans them
                    // for headers the graph never declares); and the @
                    // path itself must resolve from the post-cd cwd. The
                    // pinned task binary's rspfile slot ships the
                    // rewritten content; edges with a real ninja rspfile
                    // keep it - this fills the slot only when free.
                    if let Some(rsp_arg) = arg.strip_prefix('@') {
                        // The blanket rewrite already compensated this token
                        // for the cd depth; strip that prefix back off to
                        // get the build-root-relative file the driver reads.
                        let rsp_rel = rsp_arg
                            .strip_prefix(&"../".repeat(cd_depth))
                            .unwrap_or(rsp_arg);
                        if !rsp_rel.is_empty()
                            && !rsp_rel.starts_with('/')
                            && !rsp_rel.starts_with('-')
                            && !rsp_rel.starts_with("../")
                            && build.rspfile.is_none()
                            && extra_rspfile.is_none()
                            && self.config.build_dir.join(rsp_rel).is_file()
                        {
                            if let Ok(raw) = fs::read_to_string(self.config.build_dir.join(rsp_rel))
                            {
                                // Root-relative view for upload decisions;
                                // the shipped content gets the cd
                                // compensation instead.
                                let root_view =
                                    rewrite_ancestor_paths(&raw, &self.config.build_dir);
                                let mut _rsp_dirs = 0usize;
                                let mut _rsp_files = 0usize;
                                let is_syncqt = cmdline.contains("syncqt");
                                let mut syncqt_headers: Vec<FileId> = Vec::new();
                                for tok in root_view.split_whitespace() {
                                    // BUILD-DIR-RELATIVE TOKENS COUNT TOO.
                                    // The `../` requirement admits only paths
                                    // that climb OUT of the build dir, which
                                    // is where a GN source root sits and is
                                    // not where a CMake project's generated
                                    // headers sit. syncqt's args name both,
                                    // and a token skipped here is a header
                                    // the tool then cannot see - it runs,
                                    // succeeds, and emits an include tree
                                    // missing every private forwarding
                                    // header, which fails later in every
                                    // translation unit and names no cause.
                                    // Existence still decides: the branches
                                    // below only take a token that resolves
                                    // to a real file or directory, so
                                    // widening the shape cannot admit a
                                    // non-path.
                                    if tok.starts_with('-') || tok.starts_with('/') {
                                        continue;
                                    }
                                    if !tok.contains('/') {
                                        continue;
                                    }
                                    let p = Path::new(tok);
                                    if !p
                                        .components()
                                        .any(|c| matches!(c, std::path::Component::Normal(_)))
                                    {
                                        continue;
                                    }
                                    if p.is_dir() {
                                        _rsp_dirs += 1;
                                        for input in upload_referenced_dir(
                                            &self.rpc_client,
                                            &self.config.build_dir,
                                            p,
                                        )? {
                                            self.add_derived_file(files, input.clone());
                                            input_set.insert(input.build_path.clone(), input);
                                        }
                                    } else if p.is_file() {
                                        _rsp_files += 1;
                                        for input in upload_referenced_file(
                                            &self.rpc_client,
                                            &self.config.build_dir,
                                            p.to_path_buf(),
                                        )? {
                                            let hfid = self.add_derived_file(files, input.clone());
                                            if is_syncqt {
                                                syncqt_headers.push(hfid);
                                            }
                                            input_set.insert(input.build_path.clone(), input);
                                        }
                                    }
                                }
                                // A FORWARDING HEADER IMPLIES ITS TARGET. syncqt
                                // writes `include/<M>/x.h` as one line,
                                // `#include "/build/<src>/src/<m>/x.h"`, absolute
                                // by design, and the source header behind it is
                                // named by no edge: a translation unit that
                                // reaches the tree reads the forwarding header
                                // and dies on the target (SvgWidgets PCH via
                                // QtSvg/qtsvgglobal.h, 2026-08-23). The targets
                                // are exactly the `-headers` this rspfile names
                                // and were just uploaded, so ride them on this
                                // edge's outputs beside the tree: every consumer
                                // that expands to the tree gets its sources too,
                                // and the `.h` suffix clears the gcc filter.
                                if is_syncqt && !syncqt_headers.is_empty() {
                                    for out in build.outs() {
                                        let entry = self
                                            .co_outputs
                                            .entry(*out)
                                            .or_insert_with(|| vec![*out]);
                                        for h in &syncqt_headers {
                                            if !entry.contains(h) {
                                                entry.push(*h);
                                            }
                                        }
                                    }
                                }
                                // WHAT THE CONTENT SCAN ACTUALLY TOOK, once
                                // per rspfile. syncqt's args file names the
                                // module's source headers, and if none of
                                // them is uploaded the tool runs and emits
                                // only the forwarding headers ninja already
                                // declares - a successful task that produces
                                // an incomplete include tree, which fails
                                // later in every translation unit and names
                                // no cause. A count here separates "the scan
                                // found nothing" from "the scan was never
                                // reached", which the failure cannot.
                                eprintln!(
                                    "nix-ninja: rspfile {rsp_rel}: {} token(s), \
                                     {} dir(s), {} file(s) uploaded",
                                    root_view.split_whitespace().count(),
                                    _rsp_dirs,
                                    _rsp_files,
                                );
                                // Verbatim under the exact mirror, for the
                                // same reason the command line is: the
                                // configure-time absolute paths inside are
                                // correct as written, and the relativized
                                // form is the one syncqt discards.
                                let content = if self.config.build_dir.starts_with("/build/") {
                                    raw.clone()
                                } else {
                                    rewrite_ancestor_paths_ups(
                                        &raw,
                                        &self.config.build_dir,
                                        cd_depth,
                                    )
                                };
                                // Write under a FRESH name: the original is
                                // usually also a declared input, materialized
                                // as a read-only store symlink the task's
                                // rspfile write would then die on (EACCES,
                                // round 74 attempt 2). The @ argument keeps
                                // its (already cd-compensated) spelling plus
                                // the suffix, so nothing reads the original.
                                extra_rspfile =
                                    Some((PathBuf::from(format!("{rsp_rel}.nn-rsp")), content));
                                arg_rewrites.push((arg.clone(), format!("{arg}.nn-rsp")));
                                rewritten_idx.push(arg_i);
                            }
                        }
                        continue;
                    }
                    // An ABSOLUTE arg (bare, or a VAR=/abs value) naming a
                    // real file outside the store: rebase to a ../ chain
                    // from the build dir and upload it like any other
                    // discovered source. Every branch below guards
                    // !starts_with('/'), so without this route an edge
                    // that references its script and inputs absolutely -
                    // and declares nothing - materializes nothing. Files
                    // only: an absolute DIR here is usually the project
                    // root itself.
                    if let Some(abs) = absolute_file_candidate(&arg) {
                        let p = Path::new(abs);
                        if !p.starts_with(&self.config.store_dir)
                            && same_project_tree(&self.config.build_dir, p)
                            && p.is_file()
                        {
                            if let Some(rel) = relative_from(p, &self.config.build_dir) {
                                for input in upload_referenced_file(
                                    &self.rpc_client,
                                    &self.config.build_dir,
                                    rel,
                                )? {
                                    self.add_derived_file(files, input.clone());
                                    input_set.insert(input.build_path.clone(), input);
                                }
                            }
                        }
                    }
                    // Not a graph node - but GN commands reference source
                    // scripts the graph never declares (gcc_link_wrapper.py
                    // in every host link rule), assuming the runner shares
                    // the filesystem. A relative arg naming a real file is
                    // a task input; upload it. is_file skips -I dirs and
                    // not-yet-existing outputs; absolute and flag-shaped
                    // args never reach the check.
                    // A `VAR=relpath` value is invisible to the bare
                    // relative branch below: Path::new("INPUT_FILE=../x")
                    // is never a file. The absolute twin is handled by
                    // absolute_file_candidate above, but rewrite_cmdline
                    // rebases in-tree absolutes to ../ chains BEFORE this
                    // loop, so cmake's `-D INPUT_FILE=/build/...` arrives
                    // here respelled relative and matched nothing - the
                    // task then ran with the script uploaded (a bare -P
                    // arg) and its input absent (svt-av1's EbVersion.h,
                    // 2026-08-24, reproduced minimally before the fix).
                    if let Some((_, v)) = arg.split_once('=') {
                        if !v.starts_with('/')
                            && v.contains('/')
                            && !v.starts_with('$')
                            && Path::new(v).is_file()
                        {
                            for input in upload_referenced_file(
                                &self.rpc_client,
                                &self.config.build_dir,
                                PathBuf::from(v),
                            )? {
                                self.add_derived_file(files, input.clone());
                                input_set.insert(input.build_path.clone(), input);
                            }
                        }
                    }
                    if !arg.starts_with('-')
                        && !arg.starts_with('/')
                        && arg.contains('/')
                        && Path::new(&arg).is_file()
                    {
                        for input in upload_referenced_file(
                            &self.rpc_client,
                            &self.config.build_dir,
                            PathBuf::from(&arg),
                        )? {
                            self.add_derived_file(files, input.clone());
                            input_set.insert(input.build_path.clone(), input);
                        }
                    } else if !arg.starts_with('-')
                        && !arg.starts_with('/')
                        && arg.starts_with("../")
                        // A pure ../ chain names a ROOT, not a directory of
                        // interest: GN passes the source root itself as an
                        // argument (round 35's ../../../.. hit the upload
                        // cap, which is the cap doing its job). Require a
                        // named component after the climb.
                        && Path::new(&arg)
                            .components()
                            .any(|c| matches!(c, std::path::Component::Normal(_)))
                        && Path::new(&arg).is_dir()
                    {
                        // A relative arg naming a real SOURCE-TREE directory
                        // is the command asking for that directory's
                        // contents: dawn passes --jinja2-path, a
                        // --template-dir of jinja templates, and
                        // --markupsafe-path, and declares none of their
                        // files (upstream relies on the depfile from a
                        // PREVIOUS run plus a shared filesystem; a first
                        // run in a sandbox has neither). Bounded walk, and
                        // only for dirs that climb out of the build dir -
                        // in-build-dir args (`gen`, output dirs) are the
                        // task's own output space, not inputs.
                        for input in upload_referenced_dir(
                            &self.rpc_client,
                            &self.config.build_dir,
                            Path::new(&arg),
                        )? {
                            self.add_derived_file(files, input.clone());
                            input_set.insert(input.build_path.clone(), input);
                        }
                    } else if !arg.starts_with('-')
                        && !arg.starts_with('/')
                        && arg.starts_with("../")
                        && Path::new(&arg)
                            .extension()
                            .is_some_and(|e| e == "idl" || e == "webidl")
                        && Path::new(&arg).is_file()
                    {
                        // A schema FILE argument implies its schema
                        // DIRECTORY: json_schema_compiler resolves a
                        // cross-namespace type (usb.Device from
                        // mime_handler_private.idl) by loading the
                        // referenced namespace's schema from the same api
                        // dir at runtime, files no command line names (63
                        // failures, round 73, unmasked by the idl_parser
                        // fix). Extension-gated to .idl/.webidl - schema
                        // dirs are small and nothing else here uses those
                        // extensions - so a .json argument, which is
                        // everywhere, cannot trigger a sweep.
                        if let Some(dir) = Path::new(&arg).parent() {
                            for input in upload_referenced_dir(
                                &self.rpc_client,
                                &self.config.build_dir,
                                dir,
                            )? {
                                self.add_derived_file(files, input.clone());
                                input_set.insert(input.build_path.clone(), input);
                            }
                        }
                    } else if !arg.starts_with('-') && !arg.starts_with('/') && arg.contains('/') {
                        // GN rebases some tool arguments to the TARGET'S
                        // OWN GEN DIR rather than the build dir:
                        // ts_library's --definitions names source .d.ts
                        // files as ten-up climbs relative to the generated
                        // tsconfig, which resolve nowhere against the
                        // build dir (tsc missed MojoHandle's definition).
                        // A relative arg that matched nothing above gets
                        // one more chance against the first output's
                        // directory, existence-discriminated like every
                        // other candidate here.
                        if let Some(outdir) = outputs_hint.as_deref() {
                            let rebased = lexical_join(outdir, Path::new(&arg));
                            // A rebased arg naming a GRAPH NODE is a real
                            // dependency edge, not an opaque source: route
                            // it as a Built input (with co-outputs, below)
                            // so the generated file comes from its
                            // producing derivation rather than whatever
                            // stale copy the host build dir holds.
                            if let Some(rfid) = files.lookup(&rebased.to_string_lossy()) {
                                node_args.push(rfid);
                            } else if rebased.is_file() {
                                for input in upload_referenced_file(
                                    &self.rpc_client,
                                    &self.config.build_dir,
                                    rebased,
                                )? {
                                    self.add_derived_file(files, input.clone());
                                    input_set.insert(input.build_path.clone(), input);
                                }
                            }
                        }
                    }
                    continue;
                };
                node_args.push(fid);
                let input = match self.derived_files.get(&fid) {
                    Some(derived_file) => derived_file.clone(),
                    None => match self.build_dir_inputs.get(&fid) {
                        Some(derived_file) => derived_file.clone(),
                        None => {
                            // A graph NODE with no derived file yet is the
                            // same case as a non-node: if it names a real
                            // source file, it is a task input (the silent
                            // `continue` here dropped gcc_link_wrapper.py
                            // whenever another rule declared it as a node).
                            if !arg.starts_with('-')
                                && !arg.starts_with('/')
                                && arg.contains('/')
                                && Path::new(&arg).is_file()
                            {
                                for input in upload_referenced_file(
                                    &self.rpc_client,
                                    &self.config.build_dir,
                                    PathBuf::from(&arg),
                                )? {
                                    self.add_derived_file(files, input.clone());
                                    input_set.insert(input.build_path.clone(), input);
                                }
                            }
                            continue;
                        }
                    },
                };
                input_set.insert(input.build_path.clone(), input);
            }
        }

        // Cmdline args that named graph nodes (directly or rebased) pull
        // their producing task's co-outputs, same as ordering_ins do in
        // the worklist above: tsc follows a project-referenced tsconfig
        // to the .d.ts files written beside it.
        {
            let mut expand: Vec<FileId> = node_args;
            let mut seen_na: rustc_hash::FxHashSet<FileId> = rustc_hash::FxHashSet::default();
            while let Some(nfid) = expand.pop() {
                if !seen_na.insert(nfid) {
                    continue;
                }
                if let Some(sibs) = self.co_outputs.get(&nfid) {
                    expand.extend(sibs.iter().filter(|s| **s != nfid));
                }
                if let Some(df) = self.derived_files.get(&nfid) {
                    input_set.insert(df.build_path.clone(), df.clone());
                }
            }
        }

        // TODO: Can we avoid this? Technically the build rule isn't complete.
        //
        // Currently need this because there are rules that depend on
        // configuration phase generated files in Cpp Nix for example
        // `src/libutil/config-util.hh` which has a command like:
        // `-Isrc/libutil -include config-util.hh`.
        //
        // One way is to parse all the includes, then add it to our search
        // path above.
        //
        // BOUNDED, because at Chromium scale this blanket is a memory
        // bomb: injecting every configure-generated file into EVERY task
        // makes each derivation's input map multi-megabyte, and the
        // daemon's per-connection workers hold those derivations - seven
        // workers at ~2 GiB RSS each, measured, squeezing the machine to
        // 2.5 GiB available. Past the threshold the blanket is also
        // redundant: explicit inputs, cmdline references (matched above)
        // and depfile discovery carry the real dependencies. 512 covers
        // the meson projects this hack was written for (dbus: dozens).
        const IMPLICIT_INPUTS_LIMIT: usize = 512;
        // OVERRIDABLE, so the blanket's correctness consequences can be
        // MEASURED rather than argued. Setting the limit to 0 turns it off
        // for a whole run; unset, the constant above is the value and every
        // emitted derivation is byte-identical to what upstream emits.
        // Read once per process: this runs per task, and env::var on a hot
        // path for a value that cannot change mid-run is waste.
        static IMPLICIT_LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let implicit_limit = *IMPLICIT_LIMIT.get_or_init(|| {
            std::env::var("NIX_NINJA_IMPLICIT_INPUTS_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(IMPLICIT_INPUTS_LIMIT)
        });
        // THE BRANCH IS THE FACT, NOT THE CONSTANT. Whether the blanket
        // fires depends on build_dir_inputs.len(), which nothing prints, so
        // a hypothesis about the blanket cannot be tested without this line.
        // Once per process, naming BOTH sides of the comparison, because a
        // limit without the length it is compared against says nothing.
        static BLANKET_REPORTED: std::sync::Once = std::sync::Once::new();
        BLANKET_REPORTED.call_once(|| {
            eprintln!(
                "nix-ninja: implicit-input blanket {}: build_dir_inputs={} limit={}",
                if self.build_dir_inputs.len() <= implicit_limit {
                    "ON"
                } else {
                    "OFF"
                },
                self.build_dir_inputs.len(),
                implicit_limit,
            );
        });
        // NEVER FOR A COMPILE. A deps=gcc task discovers its headers by
        // include scanning, and the blanket on top of that makes every
        // translation unit depend on every configure-generated file -
        // including cmake_install.cmake, which carries the OUTER $out. So
        // any change to the outer derivation (a source edit, a shim edit)
        // re-keyed all 105 of qtsvg's TUs, twice, measured 2026-08-23:
        // per-TU resumability existed only as long as nothing changed.
        // Custom commands keep the blanket: they read configure files no
        // edge declares (the version-script step died without it).
        if !is_gcc_task && self.build_dir_inputs.len() <= implicit_limit {
            for input in self.build_dir_inputs.values() {
                input_set.insert(input.build_path.clone(), input.clone());
            }
        }

        lap(&NT_CMDLINE_MS);

        // Post-pass, closing the class: a .py input must ALWAYS travel
        // with its same-directory siblings, no matter which of the input
        // paths (ordering-ins, cmdline node, cmdline non-node, or a
        // derived_files HIT from an earlier task's upload) put it in the
        // set. The per-branch versions of this rule missed the HIT case:
        // the second link task found gcc_link_wrapper.py already
        // registered and attached it alone. Uploads are content-cached,
        // so re-encountering a directory is cheap.
        // Per DIRECTORY, not per file, and registered once. The per-file
        // version was the ts_library wall (5 s per task, perf round 65):
        // a closure-heavy input set holds thousands of .py files, and for
        // each one this pass re-uploaded the file (already in the set,
        // so already uploaded) and cloned the entire memoized closure
        // through add_derived_file's lossy-convert + lookup + hash. The
        // sibling set depends only on the directory; one fill registers
        // its files in self.derived_files/files for the whole run, and
        // every later task pays one contains_key per sibling.
        let mut py_dirs: rustc_hash::FxHashSet<PathBuf> = rustc_hash::FxHashSet::default();
        for i in input_set.values() {
            if i.build_path.extension().is_some_and(|e| e == "py")
                && Path::new(&i.build_path).is_file()
            {
                if let Some(dir) = i.build_path.parent() {
                    py_dirs.insert(dir.to_path_buf());
                }
            }
        }
        static PY_SIB_FILLED: std::sync::OnceLock<
            std::sync::Mutex<rustc_hash::FxHashSet<PathBuf>>,
        > = std::sync::OnceLock::new();
        let filled = PY_SIB_FILLED.get_or_init(|| std::sync::Mutex::new(Default::default()));
        for dir in py_dirs {
            let sibs = python_closure_cached(&self.rpc_client, &self.config.build_dir, &dir)?;
            let first_fill = filled.lock().unwrap().insert(dir);
            for sib in sibs.iter() {
                if first_fill {
                    self.add_derived_file(files, sib.clone());
                }
                if !input_set.contains_key(&sib.build_path) {
                    input_set.insert(sib.build_path.clone(), sib.clone());
                }
            }
        }

        // A file named INSIDE a -Wl group is an input no branch above
        // sees: the token starts with '-', so every file-arg branch skips
        // it, and json-c's version script additionally sits in the SOURCE
        // root, one level above the build dir the non-gcc blanket covers
        // (server edition 2026-08-23, ld.bfd: "cannot open linker script
        // file /build/source/json-c.sym"). Existence decides, as in the
        // rsp scan above; values of output-taking flags are skipped
        // because the linker WRITES those.
        if let Some(cmdline) = &cmdline {
            for tok in cmdline.split_whitespace() {
                let Some(group) = tok.strip_prefix("-Wl,") else {
                    continue;
                };
                for cand in wl_file_candidates(group) {
                    let p = Path::new(cand);
                    if !p.is_file() {
                        continue;
                    }
                    let up =
                        new_opaque_file(&self.rpc_client, &self.config.build_dir, p.to_path_buf())?;
                    self.add_derived_file(files, up.clone());
                    if !input_set.contains_key(&up.build_path) {
                        input_set.insert(up.build_path.clone(), up);
                    }
                }
            }
        }

        // A .template input implies its same-directory .template
        // siblings: inspector_protocol's code_generator.py loads
        // templates through a jinja FileSystemLoader rooted at the
        // module dir, and GN's per-generator input list is
        // hand-maintained - round 82 died at TemplateNotFound:
        // lib/Protocol_cpp.template, the one of eight lib templates
        // the edge forgot to declare. Same class as the schema-dir
        // rule: a declared file argument implies the directory the
        // tool actually resolves from.
        let tmpl_dirs: rustc_hash::FxHashSet<PathBuf> = input_set
            .values()
            .filter(|i| {
                i.build_path.extension().is_some_and(|e| e == "template")
                    && Path::new(&i.build_path).is_file()
            })
            .filter_map(|i| i.build_path.parent().map(Path::to_path_buf))
            .collect();
        for dir in tmpl_dirs {
            for entry in std::fs::read_dir(&dir)?.flatten() {
                let p = dir.join(entry.file_name());
                if p.extension().is_none_or(|e| e != "template")
                    || input_set.contains_key(&p)
                    || !p.is_file()
                {
                    continue;
                }
                let up = new_opaque_file(&self.rpc_client, &self.config.build_dir, p)?;
                self.add_derived_file(files, up.clone());
                input_set.insert(up.build_path.clone(), up);
            }
        }

        // grit manifests include partials and translations TEXTUALLY
        // (<part file="x.grdp">, <file path="y.xtb">), resolved relative
        // to the manifest's own directory, and GN declares none of them -
        // round 39 died at FileNotFound: address_input_strings.grdp. Same
        // worklist shape as the python-sibling pass; .grdp partials nest,
        // so found manifests re-enter the list.
        lap(&NT_PY_MS);

        let mut grd_list: Vec<PathBuf> = input_set
            .keys()
            .filter(|p| p.extension().is_some_and(|e| e == "grd" || e == "grdp"))
            .cloned()
            .collect();
        while let Some(grd) = grd_list.pop() {
            if !Path::new(&grd).is_file() {
                continue;
            }
            for r in grd_references(&grd)? {
                if input_set.contains_key(&r) {
                    continue;
                }
                let up = new_opaque_file(&self.rpc_client, &self.config.build_dir, r.clone())?;
                self.add_derived_file(files, up.clone());
                input_set.insert(up.build_path.clone(), up);
                if r.extension().is_some_and(|e| e == "grdp") {
                    grd_list.push(r);
                }
            }
            // A grd can also reference GENERATED files (a preprocessed
            // .css another task emits), which exist nowhere on disk at
            // scan time and so never pass the existence filter above.
            // A candidate that names a graph node routes as a Built
            // input with its co-outputs, same as cmdline node args.
            for cand in grd_reference_candidates(&grd)? {
                if input_set.contains_key(&cand) {
                    continue;
                }
                let Some(gfid) = files.lookup(&cand.to_string_lossy()) else {
                    continue;
                };
                let mut expand = vec![gfid];
                while let Some(nfid) = expand.pop() {
                    if let Some(df) = self.derived_files.get(&nfid) {
                        if input_set.contains_key(&df.build_path) {
                            continue;
                        }
                        input_set.insert(df.build_path.clone(), df.clone());
                        if let Some(sibs) = self.co_outputs.get(&nfid) {
                            expand.extend(sibs.iter().filter(|s| **s != nfid));
                        }
                    }
                }
            }
        }

        let mut inputs: Vec<DerivedFile> = input_set.into_values().collect();
        // Normalize away `.` components and dedup exact repeats: the same
        // file can enter input_set under two spellings of one destination
        // (`../../x` from a resolver walk, `./../../x` from an edge
        // declaration), and input_set's key cannot see they are one path.
        // nix-ninja-task then copies the file twice; fs::copy preserves
        // the store's 0444 mode, so the second copy dies EACCES on the
        // first's read-only result (css-tree/cjs/tokenizer/index.cjs,
        // round 69). Dedup is full-equality only: two DIFFERENT sources
        // claiming one destination is a real conflict and must keep
        // failing loudly rather than silently dropping one.
        for df in &mut inputs {
            if df
                .build_path
                .components()
                .any(|c| matches!(c, std::path::Component::CurDir))
            {
                df.build_path = df
                    .build_path
                    .components()
                    .filter(|c| !matches!(c, std::path::Component::CurDir))
                    .collect();
            }
        }
        inputs.sort();
        inputs.dedup();

        // Extract store paths from cmdline and add pre-extracted wrapper store paths
        lap(&NT_GRD_MS);

        // The @-argument itself resolves from the post-cd cwd; swap in the
        // compensated spelling computed by the branch above.
        let cmdline = if arg_rewrites.is_empty() {
            cmdline
        } else {
            cmdline.map(|mut c| {
                for (from, to) in &arg_rewrites {
                    c = c.replace(from.as_str(), to.as_str());
                }
                c
            })
        };
        // UNDER THE EXACT MIRROR THE TASK RUNS THE ORIGINAL COMMAND LINE.
        // The rewrite above turned configure-time absolute paths into `../`
        // chains so they would resolve in a relocated sandbox; with the
        // sandbox at the identical path they resolve as written, and the
        // rewritten form is what breaks: CMake's `file(RELATIVE_PATH)`
        // refuses a `-D` value of `../../lib` ("must be passed a full
        // path", qtsvg's prl step, 2026-08-23). Discovery above still reads
        // the rewritten view, which is what its existence checks expect;
        // only what the task EXECUTES changes. The `@rsp` respelling is
        // re-applied by argv index, since it appends a suffix to whichever
        // spelling the token had.
        let cmdline = if self.config.build_dir.starts_with("/build/") {
            match (&build.cmdline, &cmdline) {
                (Some(orig), Some(_)) => {
                    // Textual, like the rewrite above: re-joining split
                    // argv would quote `&&` and break the cd prologue.
                    let orig_args = shell_words::split(orig)?;
                    let mut c = orig.clone();
                    for i in &rewritten_idx {
                        if let Some(a) = orig_args.get(*i) {
                            c = c.replace(a.as_str(), &format!("{a}.nn-rsp"));
                        }
                    }
                    Some(c)
                }
                _ => cmdline,
            }
        } else {
            cmdline
        };

        let mut input_srcs = self.wrapper_store_paths.clone();
        if let Some(cmdline) = &cmdline {
            let found_store_paths =
                extract_store_paths(&self.config.store_dir, &self.store_regex, cmdline)?;
            input_srcs.extend(found_store_paths);
        }

        Ok(Task {
            system: self.config.system.clone(),
            wrapper_vars: self.wrapper_vars.clone(),
            input_srcs,
            build_dir: self.config.build_dir.clone(),
            build_deps: build.dependencies.clone(),
            store_dir: self.config.store_dir.clone(),
            cmdline,
            desc: build.desc.clone(),
            deps: build.deps.clone(),
            // A graph-declared rspfile wins; the discovered @-file fills
            // the slot only when the edge declares none (guarded above).
            rspfile: build
                .rspfile
                .as_ref()
                .map(|r| (r.path.clone(), r.content.clone()))
                .or(extra_rspfile),
            depfile: build.depfile.clone(),
            files: build_files,
            inputs,
            outputs,
            alias_symlinks: self.alias_symlinks.clone(),
        })
    }
}

/// End-of-run persistence for the cross-run caches; see build.rs. Public
/// because the build loop, not the runner, knows when the run is over.
pub fn resolve_cache_final_flush(rpc_client: &Arc<BuilderRpcClient>) -> Result<()> {
    crate::resolve_cache::flush()?;
    crate::resolve_cache::save_nar_stamps(&rpc_client.nar_stamps_snapshot())
}

fn build_task_derivation(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    task: Task,
) -> Result<Derivation> {
    let cmdline = match &task.cmdline {
        Some(c) => c,
        None => {
            return Err(anyhow!("Phony tasks not yet supported"));
        }
    };

    let mut drv = Derivation::new(
        "ninja-build".parse()?,
        task.system.clone().into_bytes().into(),
        format!(
            "{}/bin/nix-ninja-task",
            task.store_dir.display(&tools.nix_ninja_task)
        )
        .into_bytes()
        .into(),
    );

    // Outer output paths -> placeholders (see outer_rewrite_map), EXCEPT
    // for an LTO compile, which keeps the real paths everywhere: see
    // cmdline_is_lto for the measured reason.
    let lto_raw = task.deps.as_deref() == Some("gcc") && task_is_lto(cmdline, &task.wrapper_vars);
    let cmdline = &if lto_raw {
        cmdline.to_string()
    } else {
        rewrite_str(cmdline, &outer_rewrite_map())
    };
    drv.args.push(cmdline.to_string().into_bytes().into());

    if let Some(desc) = &task.desc {
        drv.args
            .push(format!("--description={desc}").into_bytes().into());
    }

    // Propagate wrapper environment variables to the task. They were
    // placeholdered at task creation; an LTO task gets them restored,
    // the exact inverse map, because its output cannot be restored.
    let env_restore = if lto_raw {
        outer_restore_map()
    } else {
        Vec::new()
    };
    for (key, value) in &task.wrapper_vars {
        let value = &if lto_raw {
            rewrite_str(value, &env_restore)
        } else {
            value.clone()
        };
        let final_value = if key.starts_with("NIX_CFLAGS_COMPILE") {
            // Also add a deterministic random seed based on the task's
            // cmdline for reproducible builds.
            let deterministic_seed = generate_frandom_seed(cmdline);
            format!("{value} -frandom-seed={deterministic_seed}")
        } else {
            value.clone()
        };
        drv.env.insert(
            key.clone().into_bytes().into(),
            final_value.into_bytes().into(),
        );
    }

    // Ambient environment a build system deliberately hands its tools:
    // qtwebengine invokes ninja under `cmake -E env NODEJS_EXECUTABLE=...`
    // and chromium's node.py asserts on the variable. A task derivation
    // starts from an empty environment, so the caller names what to
    // forward in NIX_NINJA_PASS_ENV (space-separated variable names) -
    // an allowlist, never the whole environment, because the whole
    // environment would put the invoking shell's noise into every drv
    // hash. A value inside the store also becomes an input so the
    // sandbox can actually exec it.
    // Every failure below is LOUD. The first version of this block skipped an
    // unset variable, a non-UTF-8 store path and an unparseable one, all in
    // silence, and set the environment variable regardless of whether the
    // matching input was added. Each of those produces a derivation that looks
    // correct and fails inside the sandbox somewhere unrelated, which is the
    // most expensive shape a build error can have.
    if let Ok(pass) = env::var("NIX_NINJA_PASS_ENV") {
        for name in pass.split_whitespace() {
            let value = env::var(name).map_err(|_| {
                anyhow!(
                    "NIX_NINJA_PASS_ENV names {name}, which is not set. \
                     An allowlist entry is a request to forward something; \
                     silently skipping it is how a typo becomes a sandbox \
                     failure thousands of tasks later."
                )
            })?;
            match Path::new(&value).strip_prefix(AsRef::<Path>::as_ref(&task.store_dir)) {
                Ok(rel) => {
                    let root = rel.components().next().ok_or_else(|| {
                        anyhow!("{name}={value} is the store root itself, not a store path")
                    })?;
                    let full = AsRef::<Path>::as_ref(&task.store_dir).join(root.as_os_str());
                    let s = full.to_str().ok_or_else(|| {
                        anyhow!("{name}={value} resolves to a non-UTF-8 store path")
                    })?;
                    let sp = task.store_dir.parse(s).map_err(|e| {
                        anyhow!("{name}={value} is under the store but unparseable: {e}")
                    })?;
                    drv.inputs.insert(SingleDerivedPath::Opaque(sp));
                }
                Err(_) if Path::new(&value).is_absolute() => {
                    // An absolute path outside the store can never be reached
                    // from inside the sandbox. Forwarding it produces a task
                    // that names a file it cannot open.
                    return Err(anyhow!(
                        "NIX_NINJA_PASS_ENV names {name}={value}, an absolute path \
                         outside {}. It would be forwarded into the task with no \
                         corresponding input, and the sandbox has no such file.",
                        AsRef::<Path>::as_ref(&task.store_dir).display()
                    ));
                }
                Err(_) => {} // not a path; forwarded as a plain value
            }
            drv.env.insert(
                name.to_string().into_bytes().into(),
                value.into_bytes().into(),
            );
        }
    }

    // Add pre-extracted store paths from cmdline and wrapper vars
    for store_path in &task.input_srcs {
        drv.inputs
            .insert(SingleDerivedPath::Opaque(store_path.clone()));
    }

    // Needed by all tasks.
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.bash.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.require_cc()?.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.coreutils.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix_ninja_task.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.patchelf.clone()));

    // Add all ninja build inputs.
    let mut input_set: HashSet<String> = HashSet::new();
    for input in &task.inputs {
        // Declare input for derivation.
        drv.inputs.insert(input.derived_path.clone());

        // Encode input for nix-ninja-task.
        let encoded = &input.to_encoded(&task.store_dir);
        input_set.insert(encoded.clone());
    }

    // Handle when rule's dep = gcc, which means we need to find all the
    // implicit header dependencies normally handled by gcc's depfiles.
    //
    // A DEPFILE WITHOUT deps=gcc IS THE SAME PROMISE IN ANOTHER SPELLING.
    // meson's nasm rule declares `depfile =` and no `deps`, so its .asm
    // sources took the no-discovery path and their %include helpers were
    // never uploaded: libvmaf's cpuid.asm died `unable to open include
    // file 'ext/x86/x86inc.asm'` with the file in the source tree (tenth
    // class, 2026-08-23). The scan over a non-source input is inert - a
    // file with no include directives contributes nothing - so the wider
    // gate can only over-declare, which is the pipeline's safe polarity.
    let mut discovered_inputs: Vec<DerivedFile> = Vec::new();
    {
        let wants_discovery = task.deps.as_deref() == Some("gcc") || task.depfile.is_some();
        if wants_discovery {
            // Only opaque inputs are processed by gcc
            let files: Vec<PathBuf> = task
                .inputs
                .iter()
                .filter_map(|input| match input.derived_path {
                    SingleDerivedPath::Opaque(_) => Some(input.build_path.clone()),
                    SingleDerivedPath::Built { .. } => None, // Will be filled in by dynamic task derivation
                })
                .collect();

            // Static analysis virtual paths: map build paths of known
            // generated inputs to themselves so canonicalize_cached
            // resolves them even if they do not yet exist on disk.
            let virtual_paths: HashMap<PathBuf, PathBuf> = task
                .inputs
                .iter()
                .filter(|i| !task.outputs.contains(&i.build_path))
                .map(|i| (i.build_path.clone(), i.build_path.clone()))
                .collect();

            let (discovered_deps, discovered_store_paths) = discover_c_includes(
                rpc_client,
                &task.store_dir,
                &task.build_dir,
                cmdline,
                files,
                Some(virtual_paths),
                task.depfile.as_deref().map(Path::new),
            )?;

            // Add discovered store paths as input sources only
            for store_path in discovered_store_paths {
                drv.inputs
                    .insert(SingleDerivedPath::Opaque(store_path.clone()));
            }

            // Add discovered deps to NIX_NINJA_INPUTS and derivation
            for derived_file in discovered_deps {
                let encoded = derived_file.to_encoded(&task.store_dir);

                // Skip if already in input_set
                if input_set.contains(&encoded) {
                    continue;
                }

                input_set.insert(encoded);
                drv.inputs.insert(derived_file.derived_path.clone());
                discovered_inputs.push(derived_file);
            }
        }
    }

    // ONE BUILD PATH, ONE CONTENT. A file the outer build regenerates
    // while discovery is scanning (gperf: automake's remake rule re-runs
    // `config.status config.h` under make -j beside the compiles) gets
    // uploaded at two different contents by two scans; both encodings
    // survive the string-keyed dedup above, and the task then dies in
    // sandbox setup: "Two different files claim one build path." Group
    // opaque inputs by build path; on a collision, upload the file AS IT
    // NOW STANDS and keep only that spelling - failing toward a fresh
    // read, never toward picking one of two stale spellings.
    {
        let mut by_bp: HashMap<PathBuf, Vec<DerivedFile>> = HashMap::new();
        for i in task.inputs.iter().chain(discovered_inputs.iter()) {
            if matches!(i.derived_path, SingleDerivedPath::Opaque(_)) {
                by_bp
                    .entry(i.build_path.clone())
                    .or_default()
                    .push(i.clone());
            }
        }
        for (bp, group) in by_bp {
            if group.len() < 2
                || group
                    .iter()
                    .all(|g| g.derived_path == group[0].derived_path)
            {
                continue;
            }
            eprintln!(
                "nix-ninja: {} was uploaded at {} different contents in one task                  (regenerated mid-build?); re-reading it now and using that alone",
                bp.display(),
                group.len(),
            );
            let abs = task.build_dir.join(&bp);
            let fresh = new_opaque_file(rpc_client, &task.build_dir, abs)?;
            for stale in &group {
                input_set.remove(&stale.to_encoded(&task.store_dir));
                drv.inputs.remove(&stale.derived_path);
            }
            input_set.insert(fresh.to_encoded(&task.store_dir));
            drv.inputs.insert(fresh.derived_path.clone());
        }
    }

    // lto_task_keeps_real_outer_paths, the input half: any input this
    // task reads that was uploaded with the placeholder substituted is
    // re-uploaded raw and swapped in. See cmdline_is_lto.
    if lto_raw {
        let rewritten = rewritten_uploads().lock().unwrap().clone();
        if !rewritten.is_empty() {
            let mut swapped = 0usize;
            for i in task.inputs.iter().chain(discovered_inputs.iter()) {
                if !matches!(i.derived_path, SingleDerivedPath::Opaque(_)) {
                    continue;
                }
                let abs = task.build_dir.join(&i.build_path);
                let Ok(canonical) = fs::canonicalize(&abs) else {
                    continue;
                };
                if !rewritten.contains(&canonical) {
                    continue;
                }
                let fresh = new_opaque_file_raw(rpc_client, &task.build_dir, abs)?;
                input_set.remove(&i.to_encoded(&task.store_dir));
                drv.inputs.remove(&i.derived_path);
                input_set.insert(fresh.to_encoded(&task.store_dir));
                drv.inputs.insert(fresh.derived_path.clone());
                swapped += 1;
            }
            if swapped > 0 {
                eprintln!(
                    "nix-ninja: LTO task keeps real outer paths in {swapped} input(s) (resumable only while $out is unchanged)"
                );
            }
        }
    }

    // The sandbox build dir must be at least as DEEP as the longest
    // `..` climb any input or command token makes: the default
    // /build/source/build is 3 deep, and Chromium's GN graph references
    // sources 5 levels up (../../../../../src/...), which escaped to
    // filesystem root and died at mkdir /src. Mirror the trailing
    // components of the REAL build dir so relative paths resolve to the
    // same names on either side.
    let cmd_climb = cmdline
        .split_whitespace()
        .map(|tok| leading_parent_components(Path::new(tok)))
        .max()
        .unwrap_or(0);
    // Computed AFTER discovery merging: discovered includes (and python
    // siblings) also climb, and a max_up taken before they were added
    // left this task a 3-deep sandbox for 5-up inputs, escaping to /.
    let max_up = task
        .inputs
        .iter()
        .map(|i| leading_parent_components(&i.build_path))
        .chain(
            discovered_inputs
                .iter()
                .map(|i| leading_parent_components(&i.build_path)),
        )
        .max()
        .unwrap_or(0)
        .max(cmd_climb);
    // EXACT MIRROR FIRST. A drop-in runs this driver inside the package's
    // own derivation, so the real build dir is already under the sandbox
    // root (`/build/<src>/build`) and the task's own /build is an empty
    // writable tree: the task can place every input at the IDENTICAL
    // absolute path. That is the only placement under which the absolute
    // paths CMake writes into generated FILES resolve - `cmake_pch.hxx`
    // opens with `#include "/build/<src>/build/include/QtSvg/QtSvg"` and
    // AutogenInfo.json carries `SRC:`/`BUILD:` roots; the trailing-
    // components mirror below rewrites command lines and cannot reach
    // file contents (qtsvg, 2026-08-23; GN emits relative paths, which
    // is why the chromium rounds never met it). Every relative input and
    // every cd-compensated `../` chain still lands, because under POSIX a
    // `..` at `/` is a no-op: a five-up alias from a two-deep dir climbs
    // to the root and re-descends into `build/<src>/...` - the same file.
    // The mirror is kept for a build dir outside /build (a dev shell).
    if task.build_dir.starts_with("/build/") {
        drv.args.push(b"--build-dir"[..].into());
        drv.args.push(
            task.build_dir
                .to_string_lossy()
                .into_owned()
                .into_bytes()
                .into(),
        );
    } else if max_up > 0 {
        let comps: Vec<_> = task.build_dir.components().collect();
        if comps.len() < max_up {
            return Err(anyhow!(
                "inputs climb {} levels above build dir {}, which has only {} components",
                max_up,
                task.build_dir.display(),
                comps.len()
            ));
        }
        let mirrored: PathBuf = comps[comps.len() - max_up..].iter().collect();
        let sandbox_build_dir = Path::new("/build/source").join(mirrored);
        drv.args.push(b"--build-dir"[..].into());
        drv.args.push(
            sandbox_build_dir
                .to_string_lossy()
                .into_owned()
                .into_bytes()
                .into(),
        );
    }

    // Sort NIX_NINJA_INPUTS to ensure determinism - BY BUILD PATH, not by
    // the encoded string. The string opens with the store path, and for a
    // Built input that is a placeholder derived from the PRODUCER'S drv
    // hash: when the producer re-keys (an autogen task after a source
    // edit), its placeholder sorts elsewhere, the RESOLVED consumer
    // derivation then carries the same input set in a different order,
    // and content-addressed early cutoff never fires - the set was equal,
    // the bytes were not (qsvgutils.cpp.o re-ran on an edit to
    // qsvgrenderer.cpp, measured 2026-08-23). The build path is stable.
    let mut inputs: Vec<String> = input_set.into_iter().collect();
    inputs.sort_by(|a, b| {
        encoded_build_path(a)
            .cmp(encoded_build_path(b))
            .then(a.cmp(b))
    });

    drv.env.insert(
        b"NIX_NINJA_INPUTS"[..].into(),
        inputs.join(" ").into_bytes().into(),
    );

    // Configure-time alias symlinks (see Runner::alias_symlinks). Encoded
    // `link=target` space-separated; alias_symlink_entry already refused
    // any path carrying a space or '='. Inserted only when non-empty so
    // graphs with no aliases keep their existing derivation hashes.
    if !task.alias_symlinks.is_empty() {
        let encoded: Vec<String> = task
            .alias_symlinks
            .iter()
            .map(|(l, t)| format!("{l}={t}"))
            .collect();
        drv.env.insert(
            b"NIX_NINJA_ALIASES"[..].into(),
            encoded.join(" ").into_bytes().into(),
        );
    }

    // UPSTREAM #17, STEP ONE: the depfile becomes a declared output.
    //
    // Nothing new is needed to carry it. `nix-ninja-task` already copies
    // every declared output out of the build dir into its placeholder, and
    // a depfile IS a build-dir-relative file the command writes - so
    // appending it to this list is the whole mechanism. What the driver then
    // gains is a real dependency list per task, which is what #17 wants to
    // read back in place of inference.
    //
    // GATED ON `deps = gcc`, NOT ON `depfile` ALONE, and the difference is
    // load-bearing. A declared output that the command does not produce
    // fails the task. `depfile` on its own is a path ninja would read IF the
    // command wrote one; `deps = gcc` is ninja's own statement that this
    // command writes a gcc-style depfile there, which is the only form that
    // guarantees the file exists when the command exits. Anything else stays
    // on the inference path it is on today, so this cannot turn a building
    // task into a failing one.
    // DEDUPED, BECAUSE A REPEATED OUTPUT COSTS A TASK FAILURE RATHER THAN A
    // WASTED COPY. `drv.outputs` is a map, so a path appearing twice collapses
    // there and the derivation looks correct; this Vec is not, so
    // NIX_NINJA_OUTPUTS carries the entry twice and nix-ninja-task copies the
    // same file to the same destination twice. `fs::copy` REPLICATES THE
    // SOURCE'S MODE, and CMake writes generated version scripts read-only, so
    // the first copy leaves a 0444 file and the second dies EACCES:
    //     copy(src/svg/Svg.version -> /nix/store/...): Permission denied
    // Measured on qtsvg 6.11.1, 2026-08-22. The message names the store as the
    // unwritable thing, which sends a reader to sandbox permissions; the
    // duplicate is invisible unless NIX_NINJA_OUTPUTS is read directly.
    // Order-preserving, because the encoded list is positional for the reader
    // on the other side.
    let mut task_outputs: Vec<PathBuf> = dedup_paths(&task.outputs);

    // SYNCQT WRITES HUNDREDS OF OUTPUTS NINJA NEVER DECLARES, and until this
    // existed no Qt module could build.
    //
    // A Qt module resolves its private includes through forwarding headers
    // syncqt generates into `include/<Module>/private/`. ninja declares the
    // TIMESTAMP file the rule touches and a handful of public headers; it
    // does not declare the rest, because syncqt decides them at runtime by
    // scanning the source tree. nix-ninja copies DECLARED outputs into store
    // paths and cannot see the others, so every translation unit that
    // includes a private header fails with
    //     fatal error: QtSvg/private/qsvghelper_p.h: No such file or directory
    // while the header sits in the build tree of the task that made it.
    // Measured on qtsvg 6.11.1, 2026-08-22, over nine harness runs.
    //
    // The DIRECTORY is the declarable unit, because its NAME is knowable at
    // graph time and its contents are not. Take it from the outputs the rule
    // already declares rather than from the command line: the public
    // forwarding headers are declared, they sit under the same
    // `include/<Module>/` root, and their common prefix is the tree syncqt
    // fills. Parsing the command's own @-file would read an argument list
    // this pass has no other reason to interpret.
    //
    // Consumers need no change: a translation unit depends on the syncqt
    // timestamp, and co-output expansion already carries a rule's siblings to
    // whoever depends on one of them - which is exactly how the PUBLIC
    // headers reach a TU's input list today. The directory rides the same
    // edge.
    // GATED ON THE OUTPUT SHAPE, NOT ON THE COMMAND, because the consumer
    // side has to reach the same conclusion from the same evidence. A
    // cmdline test here and a structural test there can disagree, and the
    // disagreement produces a DerivedFile naming a derivation output that
    // does not exist - a dangling reference rather than a missing header.
    // For a task that is not syncqt the rule declares a directory holding
    // exactly its own declared outputs, which is redundant and correct.
    for dir in undeclared_outputs(&task_outputs, task.cmdline.as_deref(), &task.build_dir) {
        if !task_outputs.contains(&dir) {
            task_outputs.push(dir);
        }
    }
    let depfile_out: Option<PathBuf> = match (&task.depfile, task.deps.as_deref()) {
        (Some(d), Some("gcc"))
            if !d.is_empty()
                && command_writes_depfile(
                    task.cmdline.as_deref(),
                    task.rspfile.as_ref().map(|(_, c)| c.as_str()),
                ) =>
        {
            let p = PathBuf::from(d);
            // An absolute or escaping depfile path is not ours to copy: the
            // task's outputs are build-dir-relative by construction, and a
            // path outside that tree would be silently rebased. Skip it and
            // leave the task exactly as it was.
            if p.is_absolute() || p.starts_with("..") {
                None
            } else {
                task_outputs.push(p.clone());
                Some(p)
            }
        }
        _ => None,
    };

    // Add all ninja build outputs.
    let mut outputs: Vec<String> = Vec::new();
    for output_path in &task_outputs {
        // Declare a content addressed output.
        let normalized_name = normalize_output(&output_path.to_string_lossy());
        drv.outputs.insert(
            normalized_name.parse()?,
            DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::NixArchive(
                harmonia_utils_hash::Algorithm::SHA256,
            )),
        );

        // Create a placeholder and encode output for nix-ninja-task.
        let placeholder = Placeholder::standard_output(
            &OutputName::from_str(&normalized_name).context("While creating placeholder")?,
        );
        // build_path keeps a ../ climb (sandbox location); rel_path maps it
        // to .nn-up so the copy lands INSIDE the output. Must agree with
        // new_built_file, which is where consumers derive the same location
        // - attempt 9 proved the two sites drift: only the consumer side
        // learned .nn-up and the producer kept copying outside $out.
        let encoded = format!(
            "{}:{}:{}",
            placeholder.render().display(),
            output_path.display(),
            store_rel_path(output_path).display(),
        );
        outputs.push(encoded);
    }
    drv.env.insert(
        b"NIX_NINJA_OUTPUTS"[..].into(),
        outputs.join(" ").into_bytes().into(),
    );

    // Name the depfile output so a consumer does not have to re-derive which
    // of the outputs it is by matching the path. #17's later steps (collect,
    // parse, cache, skip inference) read this.
    if let Some(d) = &depfile_out {
        drv.env.insert(
            b"NIX_NINJA_DEPFILE"[..].into(),
            normalize_output(&d.to_string_lossy()).into_bytes().into(),
        );
    }

    // Ninja rspfile support: the task writes this file (relative to its
    // build dir) before spawning the command. Content rides passAsFile
    // beside the input map - rsp files exist precisely because their
    // content is too large for a command line.
    let mut pass_as_file = String::from("NIX_NINJA_INPUTS NIX_NINJA_OUTPUTS");
    if let Some((rsp_path, rsp_content)) = &task.rspfile {
        drv.env.insert(
            b"NIX_NINJA_RSPFILE_PATH"[..].into(),
            rsp_path.to_string_lossy().into_owned().into_bytes().into(),
        );
        drv.env.insert(
            b"NIX_NINJA_RSPFILE_CONTENT"[..].into(),
            rewrite_str(rsp_content, &outer_rewrite_map())
                .into_bytes()
                .into(),
        );
        pass_as_file.push_str(" NIX_NINJA_RSPFILE_CONTENT");
    }

    // At Chromium scale the encoded input map is megabytes, and an env
    // var over the kernel's per-string exec limit kills the task with
    // "Argument list too long" before it starts. passAsFile is nix's
    // own mechanism for oversized attrs: the daemon writes the value
    // to a file AFTER placeholder substitution (so derived-path
    // placeholders still resolve) and hands the builder
    // NIX_NINJA_INPUTSPath / NIX_NINJA_OUTPUTSPath instead.
    drv.env
        .insert(b"passAsFile"[..].into(), pass_as_file.into_bytes().into());

    {
        // Prepare $PATH to have coreutils and bash.
        let mut path: Vec<String> = vec![
            format!("{}/bin", task.store_dir.display(&tools.bash)),
            format!("{}/bin", task.store_dir.display(tools.require_cc()?)),
            format!("{}/bin", task.store_dir.display(&tools.coreutils)),
            format!("{}/bin", task.store_dir.display(&tools.patchelf)),
        ];

        // CMake emits link and custom commands wrapped in shell no-op
        // guards: `: && <real command> && :`. The `:` is a shell builtin,
        // fine at execution time, but it is not a binary to resolve - skip
        // leading no-op tokens to find the real tool.
        //
        // A CMake CUSTOM COMMAND opens `cd <subdir> &&` instead, and `cd` is
        // also a shell builtin, so a PATH lookup for it cannot succeed: it
        // fails the whole task with "Failed to find cd: cannot find binary
        // path", which reads as a missing tool rather than as a command shape
        // this resolver does not handle. `cd_depth` above already recognises
        // this exact prefix to compute how far a post-cd relative path has to
        // climb back to the build root; only the binary pick was never taught
        // it.
        //
        // `cd` has to be stepped over as a PAIR, which is why this is a loop
        // rather than one more token in the filter: its argument is a bare
        // directory name, which is a plausible binary name and would be
        // resolved in its place, so the failure would move rather than go
        // away.
        let mut cmdline_toks = cmdline.split_whitespace();
        let cmdline_binary = loop {
            match cmdline_toks.next() {
                None => return Err(anyhow!("No command found in cmdline")),
                Some(":") | Some("&&") => continue,
                Some("cd") => {
                    cmdline_toks.next();
                    continue;
                }
                // GN quotes interpreter paths ("…/python3.14"); the shell
                // strips the quotes at exec time, so strip them here too or
                // `which` is asked for a name with literal quote characters.
                Some(tok) => break tok.trim_matches(|c| c == '"' || c == '\''),
            }
        };

        // A command resolving outside the store (e.g. a `../gen.sh` script
        // from the source tree) is a task input handled by
        // `new_opaque_file` — it reaches the sandbox as an input symlink,
        // so only store binaries become inputs and PATH entries here.
        //
        // A COMMAND CAN ALSO BE A GRAPH OUTPUT, and then `which` MUST fail:
        // orc's meson graph compiles tools/orcc in one edge and runs it as
        // the command of every .orc->.c edge (`Failed to find
        // /build/orc-0.4.42/build/tools/orcc`, server edition attempt 7,
        // 2026-08-23). The tool is already in this task's input set - the
        // cmdline node scan routes it as a Built input - so it reaches the
        // sandbox at its build-dir-relative path and the absolute-path
        // rewrite pass has already respelled the command to match. The
        // discriminator is the input set, not the error: a which failure
        // whose token matches a task input needs no store binary; one that
        // matches nothing is still the missing tool the error names.
        match which_store_path_opt(&task.store_dir, cmdline_binary) {
            Ok(Some(cmdline_path)) => {
                drv.inputs
                    .insert(SingleDerivedPath::Opaque(cmdline_path.clone()));
                path.push(format!("{}/bin", task.store_dir.display(&cmdline_path)));
            }
            Ok(None) => {}
            Err(e) => {
                let rel = relative_from(Path::new(cmdline_binary), &task.build_dir)
                    .unwrap_or_else(|| PathBuf::from(cmdline_binary));
                let is_task_input = task
                    .inputs
                    .iter()
                    .any(|i| i.build_path == rel || i.build_path == Path::new(cmdline_binary));
                if !is_task_input {
                    return Err(e);
                }
            }
        }
        // UPSTREAM #52: meson bakes `-fuse-ld=<linker>` from CC_LD/CXX_LD
        // into link lines, and gcc then execs `ld.<linker>` found on PATH -
        // a PATH this sandbox builds from cc, coreutils and the command
        // binary alone, so the link dies "cannot find ld.lld" (or silently
        // falls to bfd on toolchains where the driver only warns, which is
        // the wrong-linker case nobody notices). Resolve the requested
        // linker OUTSIDE the sandbox, where the caller's PATH still has it,
        // and carry its store path in as an input and PATH entry. Both
        // spellings are tried because the flag takes both: `ld.gold` for
        // gcc's traditional names, the bare name for mold and wild.
        for tok in cmdline.split_whitespace() {
            let Some(name) = tok.strip_prefix("-fuse-ld=") else {
                continue;
            };
            let name = name.trim_matches(|c| c == '"' || c == '\'');
            if name.is_empty() || name == "bfd" {
                continue; // bfd is binutils' own, already beside gcc
            }
            let resolved = which_store_path_opt(&task.store_dir, &format!("ld.{name}"))
                .ok()
                .flatten()
                .or_else(|| which_store_path_opt(&task.store_dir, name).ok().flatten());
            if let Some(sp) = resolved {
                path.push(format!("{}/bin", task.store_dir.display(&sp)));
                drv.inputs.insert(SingleDerivedPath::Opaque(sp));
            }
            // Unresolved: leave the command as written and let the link
            // fail with the compiler's own message, which names the linker.
        }
        drv.env
            .insert(b"PATH"[..].into(), path.join(":").into_bytes().into());
    }

    // For debugging purposes:
    // let json = serde_json::to_string_pretty(&drv)?;
    // println!("Derivation:\n{json}");

    Ok(drv)
}

// For dynamic tasks, we generate an intermediary derivation that will then
// generate the final derivation with any discovered dependencies from its
// dependencies.
//
// For example, if a task derivation depends on generated.cc, we also want
// to depend on any headers generated.cc includes but we don't know that
// without the derivation that built generated.cc also scanned for includes
// and wrote that to its $deps output.
fn build_dynamic_task_derivation(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    store_dir: &StoreDir,
    input_drv: Derivation,
    built_inputs: Vec<DerivedFile>,
) -> Result<Derivation> {
    let mut drv = Derivation::new(
        format!("{}.drv", input_drv.name).parse()?,
        input_drv.platform.clone(),
        format!("{}/bin/nix-ninja", store_dir.display(&tools.nix_ninja))
            .into_bytes()
            .into(),
    );
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix_ninja.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix.clone()));

    // Add built inputs as dependencies so the dynamic task has access to them for scanning
    for built_input in &built_inputs {
        drv.inputs.insert(built_input.derived_path.clone());
    }

    // Encode built inputs for NIX_NINJA_INPUTS so dynamic task can process them
    let mut inputs: Vec<String> = built_inputs
        .iter()
        .map(|input| input.to_encoded(store_dir))
        .collect();
    // Same ordering rule as build_task_derivation, same reason.
    inputs.sort_by(|a, b| {
        encoded_build_path(a)
            .cmp(encoded_build_path(b))
            .then(a.cmp(b))
    });
    drv.env.insert(
        b"NIX_NINJA_INPUTS"[..].into(),
        inputs.join(" ").into_bytes().into(),
    );
    // Same E2BIG hazard as the task derivation: see the passAsFile
    // comment in build_task_derivation.
    drv.env
        .insert(b"passAsFile"[..].into(), b"NIX_NINJA_INPUTS"[..].into());

    drv.outputs.insert(
        "out".parse()?,
        DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::Text),
    );
    drv.env.insert(
        b"out"[..].into(),
        Placeholder::standard_output(&OutputName::from_str("out")?)
            .render()
            .into_os_string()
            .into_encoded_bytes()
            .into(),
    );

    // Add the dynamic-task subtool argument
    drv.args.push(b"-t"[..].into());
    drv.args.push(b"dynamic-task"[..].into());

    // builder-rpc-v0 does not auto-export $name, but the outer-output rename needs it.
    let dyn_drv_name = format!("{}.drv", input_drv.name);
    drv.env
        .insert(b"name"[..].into(), dyn_drv_name.into_bytes().into());

    // Propagate sources to dynamic task for it discover inputs.
    let src = env::var("src").map_err(|_| anyhow!("Expected $src to be set"))?;
    drv.env
        .insert(b"src"[..].into(), src.clone().into_bytes().into());
    let src_store_path: StorePath = store_dir.parse(&src)?;
    drv.inputs
        .insert(SingleDerivedPath::Opaque(src_store_path.clone()));

    // Set up PATH to include nix binary
    let path = format!("{}/bin", store_dir.display(&tools.nix));
    drv.env.insert(b"PATH"[..].into(), path.into_bytes().into());

    // Requires extra experimental features to add our derivations.
    drv.env.insert(
        b"NIX_CONFIG"[..].into(),
        b"extra-experimental-features = nix-command ca-derivations dynamic-derivations"[..].into(),
    );

    // Require builder-rpc-v0 so the dynamic-task drv runs in a sandbox
    // where the daemon socket is bind-mounted in restricted mode (see
    // NixOS/nix#15793). nix-ninja's inside-sandbox calls then go through
    // the worker protocol's restricted allowlist instead of recursive-nix.
    drv.env.insert(
        b"requiredSystemFeatures"[..].into(),
        b"builder-rpc-v0"[..].into(),
    );

    // Serialize the derivation and add it to the nix store
    let drv_json = serde_json::to_string(&input_drv)?;
    let drv_json_name = format!("drv-{}.json", input_drv.name);
    let drv_json_path = rpc_client.add_to_store_text(&drv_json_name, drv_json.as_bytes())?;

    // Add derivation.json as input dependency and argument
    drv.inputs
        .insert(SingleDerivedPath::Opaque(drv_json_path.clone()));
    drv.args.push(
        store_dir
            .display(&drv_json_path)
            .to_string()
            .into_bytes()
            .into(),
    );

    Ok(drv)
}

/// Handles the result of build_task_derivation, deciding whether to wrap with
/// a dynamic task derivation or use the derivation directly.
fn handle_derivation_result(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    task: Task,
    mut drv: Derivation,
    config: &RunnerConfig,
) -> Result<SingleDerivedPath> {
    // This function sits OUTSIDE new_task and therefore outside RESOLVE_MS,
    // and it blocks the serial resolve loop on a daemon realise per dynamic
    // task. Round 86 reported 142 s of "total resolve time" against ~43
    // minutes of wall clock at the same task count, and the whole gap was
    // here - unmeasured, so three rounds were tuned against memory instead.
    // Timed with a Drop guard for the same reason build_paths is: several
    // early returns, and an untimed one hides the case worth catching.
    let _dyn_timer = DynDiscoveryTimer(std::time::Instant::now());
    // Collect built inputs when deps == "gcc" for dynamic dependency discovery
    let built_inputs: Vec<DerivedFile> = if task.deps.as_ref() == Some(&"gcc".to_string()) {
        task.inputs
            .iter()
            .filter(|input| matches!(input.derived_path, SingleDerivedPath::Built { .. }))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    if !built_inputs.is_empty() {
        // If we're in Nix sandbox, create a dynamic derivation to handle
        // dynamic dependencies.
        if config.is_output_derivation {
            let dynamic_drv = build_dynamic_task_derivation(
                tools.clone(),
                rpc_client,
                &config.store_dir,
                drv,
                built_inputs,
            )?;
            let dynamic_drv_path = rpc_client.add_drv_to_store(&config.store_dir, &dynamic_drv)?;
            Ok(SingleDerivedPath::Built {
                drv_path: Arc::new(SingleDerivedPath::Opaque(dynamic_drv_path)),
                output: "out".parse().unwrap(),
            })
        } else {
            // Otherwise, symlink these built_inputs into build_dir and do
            // dependency discovery locally.

            // dyn is ~82% of this campaign's wall clock and had no breakdown
            // until now - the same omission one level down that made the
            // whole phase invisible. Its parts have different fixes: realise
            // is a daemon round trip (memoized), discovery runs the compiler
            // locally (real work). Naming which dominates decides whether
            // anything is left to optimize here or whether it is the build.
            let t_realise = std::time::Instant::now();
            let built_paths =
                local::build_derived_files(rpc_client, &config.store_dir, &built_inputs)?;
            DYN_REALISE_MS.fetch_add(
                t_realise.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );

            let t_discover = std::time::Instant::now();
            let (discovered_deps, discovered_store_paths) =
                dynamic_task::discover_dynamic_dependencies(
                    rpc_client,
                    &config.store_dir,
                    &config.build_dir,
                    &drv,
                    built_paths,
                )?;
            DYN_DISCOVER_MS.fetch_add(
                t_discover.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );

            // THE REMAINDER OF dyn WAS UNNAMED, AND AT TASK 15,500 OF ROUND 90
            // IT WAS 33% OF IT: realise 3172 s + discover 750 s against dyn
            // 5869 s. That is addendum 721's shape arriving one level down -
            // there, handle_derivation_result sat outside RESOLVE_MS and the
            // driver printed its smallest phase as the total. Two candidates
            // sit here and they have DIFFERENT fixes, which is the whole
            // reason for measuring rather than picking:
            //
            //   update_derivation_with_discoveries rewrites the input set,
            //   and the input set is growing superlinearly - measured within
            //   round 90 alone, tasks x1.6 from 9,500 to 15,500 while realise
            //   asked x92. Its fix is depsets (plan item 2).
            //
            //   add_drv_to_store is a DAEMON ROUND TRIP per dynamic task,
            //   the same class as the call 721 found. Its fix is a memo or a
            //   batch, not depsets.
            //
            // Naming them separately is what stops the next round tuning the
            // wrong one, which this campaign has already done twice.
            // USED AGAINST DECLARED. The DENOMINATOR is counted here and
            // the numerator inside discover_c_includes, because the two sets
            // have to be drawn from one population or the ratio restates a
            // filter instead of measuring anything. `built_inputs` is what
            // discovery is actually offered - the whole of it, before any
            // filtering - so it is the honest denominator; `task.inputs` is
            // wider and would inflate the prunable share with inputs that
            // were never candidates.
            //
            // Restricted to header-shaped inputs: pruning object files or
            // generated sources is a different change with a different risk,
            // and folding them into one ratio would price neither.
            for input in &built_inputs {
                if header_like(&input.build_path) {
                    DECLARED_HEADERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }

            let t_update = std::time::Instant::now();
            dynamic_task::update_derivation_with_discoveries(
                &mut drv,
                discovered_deps,
                discovered_store_paths,
                &config.store_dir,
            )?;
            DYN_UPDATE_MS.fetch_add(
                t_update.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            DYN_UPDATE_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let t_addrv = std::time::Instant::now();
            let drv_path = rpc_client.add_drv_to_store(&config.store_dir, &drv)?;
            DYN_ADDDRV_MS.fetch_add(
                t_addrv.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            DYN_ADDDRV_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(SingleDerivedPath::Opaque(drv_path))
        }
    } else {
        // THE ARM THAT WAS NEVER TIMED, AND ON THREE EDITION LOGS IT IS THE
        // ONLY ARM THAT RAN. The four sub-timers above all sit inside the
        // dynamic arm, while the DynDiscoveryTimer guard at the top of this
        // function spans BOTH, so every task taking this branch landed in
        // `dyn` with nothing to account for it. ArtNix read dyn growing
        // 0.084 s/task on 2026-08-29 with realise, discover, update and
        // adddrv all zero and `realise 0/0 sent` on every line, which is this
        // arm running 4000 times with one untimed daemon round trip in it.
        //
        // Do not read the number off that slope: it is an elimination, not a
        // measurement. This pair is what makes it a measurement, and it is
        // reported separately from DYN_ADDDRV_* on purpose - same call, two
        // populations, and pooling them would hide which one costs.
        let t_add = std::time::Instant::now();
        let drv_path = rpc_client.add_drv_to_store(&config.store_dir, &drv)?;
        DYN_PLAIN_ADDDRV_MS.fetch_add(
            t_add.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        DYN_PLAIN_ADDDRV_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(SingleDerivedPath::Opaque(drv_path))
    }
}

pub fn which_store_path(store_dir: &StoreDir, binary_name: &str) -> Result<StorePath> {
    which_store_path_opt(store_dir, binary_name)?.ok_or_else(|| {
        anyhow!(
            "{} resolved to a path outside the Nix store ({})",
            binary_name,
            store_dir
        )
    })
}

/// Like [`which_store_path`], but returns `None` for a binary that resolves
/// outside the Nix store (e.g. a script in the source tree).
fn which_store_path_opt(store_dir: &StoreDir, binary_name: &str) -> Result<Option<StorePath>> {
    let binary_path =
        which(binary_name).map_err(|err| anyhow!("Failed to find {}: {}", binary_name, err))?;

    // Canonicalize will resolve all symlinks and return an absolute path
    let canonical_path = std::fs::canonicalize(&binary_path).with_context(|| {
        format!(
            "canonicalize {} (from PATH lookup of {})",
            binary_path.display(),
            binary_name
        )
    })?;

    if !canonical_path.starts_with(store_dir) {
        return Ok(None);
    }

    let store_path_dir = canonical_path
        .parent() // Get bin/ directory
        .and_then(|p| p.parent()) // Get the store path ($out)
        .ok_or_else(|| anyhow!("Cannot determine store path from binary: {}", binary_name))?
        .to_str()
        .context("Path was not valid UTF-8")?;

    store_dir
        .parse(store_path_dir)
        .context("Failed to parse store path")
        .map(Some)
}

fn extract_store_paths(
    store_dir: &StoreDir,
    store_regex: &Regex,
    s: &str,
) -> Result<Vec<StorePath>> {
    let mut store_paths = Vec::new();
    for cap in store_regex.find_iter(s) {
        let store_path: StorePath = store_dir.parse(cap.as_str())?;
        if store_path.is_derivation() {
            continue;
        }
        if !store_path.to_absolute_path(store_dir).exists() {
            continue;
        }
        // THE OUTER DERIVATION'S OWN OUTPUTS ARE NEVER INPUTS. They do not
        // exist during the build phase, so the check above skipped them
        // and nothing noticed; at install time they exist on disk and are
        // not yet valid, so declaring one is an AddToStore the daemon
        // refuses ("path ... is not valid": alsa-lib, make install
        // recompiling aserver.o, 2026-08-23). Same list remove_outer_rpath
        // reads.
        if outer_output_paths()
            .iter()
            .any(|o| o == &store_path.to_absolute_path(store_dir).to_string_lossy())
        {
            continue;
        }
        store_paths.push(store_path);
    }
    Ok(store_paths)
}

/// THE OUTER OUTPUT PATHS ARE REWRITTEN TO SAME-LENGTH PLACEHOLDERS IN
/// EVERYTHING A TASK SEES, AND BACK IN EVERYTHING IT PRODUCES.
///
/// `$out` of the outer derivation moves with every change to that
/// derivation, a one-line source edit included, and configure writes it
/// into generated headers (`ALSA_CONFIG_DIR` in alsa-lib's config.h, which
/// every TU includes) and into command lines. Measured 2026-08-23: one
/// edited .c file rebuilt 110 of 116 translation units. This is the same
/// problem Nix solves for content-addressed outputs the same way: a
/// placeholder of identical length stands in during the build, and the
/// real path is substituted into the result afterwards. Same length
/// because the substitution has to be valid inside an object file.
///
/// The task derivation and its output carry ONLY the placeholder, which is
/// what makes the derivation stable and its cached output reusable under
/// a different `$out`; the driver alone holds the mapping and applies it
/// when it materializes an output into the build tree (`local.rs`).
pub fn outer_rewrite_map() -> Vec<(String, String)> {
    let store_dir = std::env::var("NIX_STORE").unwrap_or_else(|_| "/nix/store".into());
    std::env::var("outputs")
        .unwrap_or_else(|_| "out".into())
        .split_whitespace()
        .filter_map(|name| std::env::var(name).ok().map(|p| (name.to_string(), p)))
        .filter_map(|(name, real)| {
            // /nix/store/<32 base32 chars>-<rest>
            let rel = real.strip_prefix(&format!("{store_dir}/"))?;
            let (hash, rest) = rel.split_once('-')?;
            if hash.len() != 32 {
                return None;
            }
            let digest = Sha256::digest(format!("nix-ninja-outer-output:{name}").as_bytes());
            let alphabet = b"0123456789abcdfghijklmnpqrsvwxyz";
            let fake: String = digest
                .iter()
                .take(32)
                .map(|b| alphabet[(*b as usize) % 32] as char)
                .collect();
            Some((real.clone(), format!("{store_dir}/{fake}-{rest}")))
        })
        .collect()
}

/// Apply `map` (from -> to) to `s`; a no-op where nothing matches.
pub fn rewrite_str(s: &str, map: &[(String, String)]) -> String {
    let mut out = s.to_string();
    for (from, to) in map {
        if out.contains(from.as_str()) {
            out = out.replace(from.as_str(), to);
        }
    }
    out
}

/// Byte-wise form for file contents, which may be binary.
pub fn rewrite_bytes(data: &[u8], map: &[(String, String)]) -> Option<Vec<u8>> {
    let mut out = data.to_vec();
    let mut changed = false;
    for (from, to) in map {
        let (f, t) = (from.as_bytes(), to.as_bytes());
        debug_assert_eq!(f.len(), t.len());
        let mut i = 0;
        while i + f.len() <= out.len() {
            if &out[i..i + f.len()] == f {
                out[i..i + f.len()].copy_from_slice(t);
                changed = true;
                i += f.len();
            } else {
                i += 1;
            }
        }
    }
    changed.then_some(out)
}

/// The reverse map: placeholder -> real.
pub fn outer_restore_map() -> Vec<(String, String)> {
    outer_rewrite_map()
        .into_iter()
        .map(|(r, p)| (p, r))
        .collect()
}

/// The outer derivation's output paths, from the `outputs` env var the
/// builder is given (`$out`, `$dev`, ...). Empty outside a build.
fn outer_output_paths() -> Vec<String> {
    std::env::var("outputs")
        .unwrap_or_else(|_| "out".into())
        .split_whitespace()
        .filter_map(|o| std::env::var(o).ok())
        .collect()
}

/// Concurrent `new_opaque_file` over a batch, order-preserving.
///
/// Each add is a daemon round trip (NAR upload or a dedup-cache hit), so a
/// task with N undeclared includes paid N sequential round trips on its
/// critical path. Upstream #18 names exactly this. Bounded scoped threads,
/// mirroring the include scanner's own pattern; the client's connection
/// pool is the daemon-side bound, this const only caps threads per task.
/// Duplicates are NOT deduped: a duplicate include previously produced a
/// duplicate DerivedFile entry, and this helper must not change what the
/// driver emits - an emission change moves every banked hash mid-campaign.
/// The client's stamp cache makes the duplicate upload a map lookup anyway.
/// The elements of one `-Wl,` group (already stripped of the prefix)
/// that could name input FILES: flags and the values of output-taking
/// flags are excluded, everything else is a candidate for the caller's
/// existence check. `--flag=value` and `--flag,value` both parse.
fn wl_file_candidates(group: &str) -> Vec<&str> {
    const OUTPUT_FLAGS: [&str; 4] = ["--dependency-file", "-Map", "--Map", "--out-implib"];
    let mut out = Vec::new();
    let mut skip_next = false;
    for el in group.split(',') {
        if skip_next {
            skip_next = false;
            continue;
        }
        let (flag, val) = match el.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (el, None),
        };
        if OUTPUT_FLAGS.contains(&flag) {
            if val.is_none() {
                skip_next = true;
            }
            continue;
        }
        let cand = val.unwrap_or(el);
        if !cand.is_empty() && !cand.starts_with('-') {
            out.push(cand);
        }
    }
    out
}

/// Uniquifies every temp copy this driver writes; see the nn-outer and
/// shebang sites for the race a pid-only name loses.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn new_opaque_files(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &std::path::Path,
    paths: Vec<PathBuf>,
) -> Result<Vec<DerivedFile>> {
    const PER_TASK_ADDS: usize = 8;
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(PER_TASK_ADDS) {
        let results: Vec<Result<DerivedFile>> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|p| scope.spawn(move || new_opaque_file(rpc_client, build_dir, p.clone())))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(anyhow!("upload thread panicked")))
                })
                .collect()
        });
        for r in results {
            out.push(r?);
        }
    }
    Ok(out)
}

fn new_opaque_file(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &std::path::Path,
    path: PathBuf,
) -> Result<DerivedFile> {
    let relative_path = relative_from(&path, build_dir).unwrap_or(path);
    let mut path = relative_path
        .to_str()
        .context("Path was not valid UTF-8")?
        .to_owned();
    canon::canonicalize_path(&mut path);

    let canonical_path = fs::canonicalize(&path).with_context(|| format!("canonicalize {path}"))?;

    // builder-rpc-v0 restricts the allowlist; `nix store add` issues
    // AddTempRoot which the daemon refuses. Upload as a NAR-hashed CA
    // object instead, which (unlike a text-CA object) preserves the file
    // mode, notably the executable bit.
    // Route through normalize_output: a raw file_name() containing a
    // character outside the store-name grammar (e.g. `@`) is refused by
    // AddToStore. See normalize_output for the grammar and why the lossy
    // map is sound.
    let name = canonical_path
        .file_name()
        .map(|n| normalize_output(&n.to_string_lossy()))
        .unwrap_or_else(|| "source".to_string());
    // An executable whose shebang is `#!/usr/bin/env <prog>` cannot be
    // EXEC'd inside the task sandbox, which has no /usr/bin/env: protoc
    // execs protoc-gen-ts_proto.py as a plugin and reports "not found
    // or is not executable" (round 70). This is nixpkgs' patchShebangs
    // problem, solved the same way at the same layer: upload a copy
    // whose shebang names the interpreter's resolved store path. Done
    // here rather than in nix-ninja-task deliberately - the task binary
    // is part of every banked derivation's identity - and only the
    // affected store objects re-key. Scripts INVOKED as `python3 x.py`
    // never read their shebang, so the rewrite is inert for them.
    let upload_src = patched_env_shebang(&canonical_path)?;
    // A file that names an outer output path is uploaded with the path
    // rewritten to its placeholder (config.h carrying $out/share/alsa),
    // so the upload's hash, and every task reading it, is stable across
    // changes to the outer derivation. Regular files only; a directory
    // is uploaded as it is.
    let upload_src = if upload_src.is_none() && canonical_path.is_file() {
        let map = outer_rewrite_map();
        let data = fs::read(&canonical_path)?;
        match rewrite_bytes(&data, &map) {
            Some(rewritten) => {
                // UNIQUE PER CALL, not per pid: batched adds run this
                // concurrently, and two uploads of one file racing a
                // pid-keyed tmp name hand the daemon a truncated stream
                // ("string is too long" / "reached end of FramedSource",
                // gperf 2026-08-23) - and the truncated moment is also
                // where the empty-config.h spelling came from.
                let tmp = std::env::temp_dir().join(format!(
                    "nn-outer-{}-{}-{}",
                    std::process::id(),
                    TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    canonical_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
                fs::write(&tmp, rewritten)?;
                if let Ok(md) = fs::metadata(&canonical_path) {
                    let _ = fs::set_permissions(&tmp, md.permissions());
                }
                // Remembered, so an LTO task can ask for the raw form
                // (lto_task_keeps_real_outer_paths).
                rewritten_uploads()
                    .lock()
                    .unwrap()
                    .insert(canonical_path.clone());
                Some(tmp)
            }
            None => None,
        }
    } else {
        upload_src
    };
    let store_path = rpc_client.add_to_store_nar_cached(
        &name,
        upload_src.as_deref().unwrap_or(&canonical_path),
        &canonical_path,
    )?;

    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None, // None for opaque files - store path points directly to file
    })
}

/// Canonical paths whose upload carried an outer output path and was
/// therefore uploaded with the placeholder substituted. An LTO task must
/// not read those (see lto_task_keeps_real_outer_paths).
fn rewritten_uploads() -> &'static std::sync::Mutex<HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// The RAW upload of a build-dir file: no outer-output placeholder, and
/// past the NAR stamp cache, which is keyed by canonical path and would
/// otherwise hand back the placeholder-carrying object for the same
/// path. Content-addressed on the daemon side, so a repeat costs one
/// dedup round trip.
fn new_opaque_file_raw(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &std::path::Path,
    path: PathBuf,
) -> Result<DerivedFile> {
    let relative_path = relative_from(&path, build_dir).unwrap_or(path);
    let mut path = relative_path
        .to_str()
        .context("Path was not valid UTF-8")?
        .to_owned();
    canon::canonicalize_path(&mut path);
    let canonical_path = fs::canonicalize(&path).with_context(|| format!("canonicalize {path}"))?;
    let name = canonical_path
        .file_name()
        .map(|n| normalize_output(&n.to_string_lossy()))
        .unwrap_or_else(|| "source".to_string());
    let upload_src = patched_env_shebang(&canonical_path)?;
    let store_path =
        rpc_client.add_to_store_nar(&name, upload_src.as_deref().unwrap_or(&canonical_path))?;
    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None,
    })
}

/// Does this compile emit LTO bytecode? The last of `-flto*` / `-fno-lto`
/// on the line wins, as it does for gcc. `-ffat-lto-objects` changes
/// nothing here: the linker still consumes the IR half.
///
/// WHY IT MATTERS - lto_task_keeps_real_outer_paths. The outer-output
/// placeholder is restored in task OUTPUTS by an equal-length byte
/// rewrite (rewrite_bytes, local.rs). That is sound for a plain object,
/// whose literals sit verbatim in .rodata, and UNSOUND for LTO
/// bytecode: gcc streams the IR through a compressed, checksummed
/// section, so the placeholder is invisible to the byte search at the
/// default level and the object is corrupted by the rewrite at
/// -flto-compression-level=0 ("compressed stream: data error" from
/// lto1, measured 2026-08-24). Nothing downstream can repair it either:
/// the link that consumes the IR embeds the placeholder into the final
/// binary, where it names a path that does not exist. bison under the
/// make drop-in shipped PKGDATADIR as the placeholder and failed 698 of
/// 744 tests, twice, the first time misattributed to a materialization
/// window (the local.rs comment); the window was real and was not the
/// cause. So an LTO compile task never sees a placeholder at all: real
/// paths on its command line and in its environment, and a raw upload
/// of every input the global rewrite touched. Such a task re-keys when
/// the outer output path moves, which is the honest cost - only tasks
/// that actually read an outer path pay it, and a wrong artifact is not
/// a cache hit.
// Exercised by this module's tests and named by the comments at :2166 and
// :2408 that explain why LTO tasks are handled the way they are. dead_code
// fires because the non-test callers went away when that handling moved, not
// because the predicate stopped being the documented one - deleting it would
// delete the tests that pin the behaviour those comments describe.
#[allow(dead_code)]
fn cmdline_is_lto(cmdline: &str) -> bool {
    scan_lto_flags(cmdline, false)
}

/// Last-wins scan of `-flto*` / `-fno-lto` over one flag string, from a
/// starting state.
fn scan_lto_flags(flags: &str, start: bool) -> bool {
    let mut lto = start;
    for tok in flags.split_whitespace() {
        if tok == "-fno-lto" {
            lto = false;
        } else if tok == "-flto" || tok.starts_with("-flto=") {
            lto = true;
        }
    }
    lto
}

/// The compiler's view, not the command line's. A stdenv can inject
/// -flto from INSIDE its cc wrapper (nixpkgs `cc-cflags-before`), where
/// neither the ninja edge nor the task env shows it, so a task whose
/// command reads `gcc -O2 -c` is LTO all the same. The wrapper's order
/// decides precedence and is reproduced here: the wrapper's own
/// baseline first (NIX_NINJA_ASSUME_LTO=1, set by a stdenv that injects
/// -flto globally), then the command line, then NIX_CFLAGS_COMPILE,
/// which the wrapper appends AFTER the command line - so a package's
/// -fno-lto opt-out delivered that way wins, exactly as it does for gcc.
fn task_is_lto(cmdline: &str, wrapper_vars: &HashMap<String, String>) -> bool {
    let assume = std::env::var("NIX_NINJA_ASSUME_LTO")
        .map(|v| v == "1")
        .unwrap_or(false);
    let after_cmdline = scan_lto_flags(cmdline, assume);
    match wrapper_vars.get("NIX_CFLAGS_COMPILE") {
        Some(extra) => scan_lto_flags(extra, after_cmdline),
        None => after_cmdline,
    }
}

/// Where `path` is an executable regular file opening with
/// `#!/usr/bin/env <prog>` (single word, no env options), write a copy
/// whose shebang is the PATH-resolved absolute location of <prog> and
/// return it; None leaves the upload untouched. Resolution failure and
/// exotic shebangs (`env -S`, arguments) fail toward the original file,
/// whose failure names itself in the task log; a wrong rewrite would
/// fail strangely. The copy keeps the executable bit, which is the
/// property the whole exercise exists to make usable.
fn patched_env_shebang(path: &Path) -> Result<Option<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;
    let md = fs::metadata(path)?;
    if !md.is_file() || md.permissions().mode() & 0o111 == 0 {
        return Ok(None);
    }
    let body = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let Some(first_nl) = body.iter().position(|&b| b == b'\n') else {
        return Ok(None);
    };
    let Ok(first) = std::str::from_utf8(&body[..first_nl]) else {
        return Ok(None);
    };
    let Some(prog) = first.strip_prefix("#!/usr/bin/env ") else {
        return Ok(None);
    };
    let prog = prog.trim();
    if prog.is_empty() || prog.contains(' ') || prog.starts_with('-') {
        return Ok(None);
    }
    let Some(resolved) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|d| {
            let c = d.join(prog);
            c.is_file().then_some(c)
        })
    }) else {
        return Ok(None);
    };
    // Memoized per source path: the same script is uploaded once per
    // walk memo miss, but several scripts share interpreters and the
    // tmp copies are tiny; keyed by source so repeat calls are free.
    static SHEBANG_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, PathBuf>>> =
        std::sync::OnceLock::new();
    let memo = SHEBANG_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(path) {
        return Ok(Some(hit.clone()));
    }
    let dir = std::env::temp_dir().join(format!("nix-ninja-shebang-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    // TMP_SEQ, not memo.len(): two concurrent misses could draw the same
    // length and write one file from two threads (same race as the
    // nn-outer copies).
    let out = dir.join(format!(
        "{}-{}",
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("script")
    ));
    let mut patched = format!("#!{}\n", resolved.display()).into_bytes();
    patched.extend_from_slice(&body[first_nl + 1..]);
    fs::write(&out, patched)?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o755))?;
    memo.lock().unwrap().insert(path.to_path_buf(), out.clone());
    Ok(Some(out))
}

/// An ABSOLUTE build_path is a sandbox escape: nix-ninja-task joins it
/// onto its build dir with Path::join, whose semantics DISCARD the
/// prefix for absolute paths, so the task then mkdirs the host path
/// inside the sandbox and dies EACCES (CMake's ${cmake_ninja_workdir}
/// and GN actions both emit absolute workdir paths; meson never does,
/// which is why five packages built before Chromium's graph hit this).
/// Relativize against the build dir; refuse LOUDLY anything that
/// escapes it rather than silently relocating an unknown path.
/// Number of leading `..` components of a relative path: how many
/// levels above its base directory it reaches before descending.
/// Rewrite absolute paths under any ancestor of the build dir into
/// build-dir-relative ones. Meson bakes `files(...)` results and GN bakes
/// arguments like gen_icui18n_shim's `--headers-root` as absolute host
/// paths; neither exists inside the task sandbox, where the source tree is
/// materialized at build-dir-relative locations. Each occurrence of
/// "<ancestor>/" becomes the "../" chain reaching it from the build dir.
/// Deepest first, because a deeper prefix contains every shallower one;
/// stops above 3 path components so "/home/", "/nix/" and "/" are never
/// rewritten. The cmd_climb scan below the discovery loop keeps the sandbox
/// deep enough for whatever "../" chains this emits.
fn rewrite_ancestor_paths(cmdline: &str, build_dir: &Path) -> String {
    rewrite_ancestor_paths_ups(cmdline, build_dir, 0)
}

/// The whole-command rewrite. A CMake custom command opens with
/// `cd <subdir> && rest`, and every path in `rest` - arguments, @-files,
/// `cmake -E touch` targets - resolves from that subdir, not the build
/// root (round 74: syncqt's @-file and then the touch beside it, one
/// re-launch each). Rewrite the cd target root-relative and the rest
/// with the subdir's depth compensated; commands with no cd prologue
/// keep the plain rewrite.
fn rewrite_cmdline(cmdline: &str, build_dir: &Path) -> String {
    if let Some(after_cd) = cmdline.strip_prefix("cd ") {
        if let Some((dir, tail)) = after_cd.split_once(" && ") {
            let rel = rewrite_ancestor_paths(dir, build_dir);
            if !rel.starts_with('/') && !rel.starts_with("../") {
                let depth = Path::new(&rel)
                    .components()
                    .filter(|c| matches!(c, std::path::Component::Normal(_)))
                    .count();
                return format!(
                    "cd {rel} && {}",
                    rewrite_ancestor_paths_ups(tail, build_dir, depth)
                );
            }
            // A CD TARGET ABOVE THE BUILD DIR: DROP THE PROLOGUE RATHER
            // THAN COMPENSATE FOR IT.
            // CMake configured out-of-source at <src>/build emits rules
            // that cd into the SOURCE tree, and those used to fall through
            // to the plain rewrite - which rewrites the tail build-dir
            // relative AND leaves the cd in place, so every path resolved
            // from the wrong directory:
            //     cd ../src/svg && cmake -DIN_FILE=src/svg/Svg.version.in ...
            // `Svg.version.in` is generated into the build dir, so CMake
            // reported "Input file ... doesn't exists" - a missing INPUT,
            // which is not what was wrong.
            // The tail is ALREADY correct for a process running in the
            // build dir, because that is what the plain rewrite produces.
            // So the prologue is not merely uncompensated, it is the
            // defect: remove it and the command is right.
            // WHY NOT RE-EXPRESS THE TAIL AGAINST THE CD TARGET, which is
            // the other obvious repair and was tried first: it changes the
            // SPELLING of every path in the command, and several passes
            // below turn command tokens into input build paths by reading
            // that spelling. Measured 2026-08-22 - every input in a qtsvg
            // task registered TWICE, once correctly and once at a path one
            // component off, and the task then died materialising two
            // symlinks at one destination. Changing the emitted paths is a
            // wide change; deleting three characters is a narrow one, and
            // both fix the same command.
            // Bounded to a target with no absolute prefix, and the tail is
            // rewritten exactly as the fallback below would rewrite it, so
            // nothing downstream sees a spelling it has not always seen.
            if !rel.starts_with('/') {
                return rewrite_ancestor_paths(tail, build_dir);
            }
        }
    }
    rewrite_ancestor_paths(cmdline, build_dir)
}

/// Like rewrite_ancestor_paths, but every emitted `../` chain gets
/// `extra_ups` more levels. For content resolved by a process whose cwd
/// is `extra_ups` components BELOW the build dir (a `cd <subdir> &&`
/// command prologue), the compensation is what makes the rewritten
/// relative paths land where the absolute originals pointed.
fn rewrite_ancestor_paths_ups(cmdline: &str, build_dir: &Path, extra_ups: usize) -> String {
    let mut cmdline = cmdline.to_string();
    let mut ups = extra_ups;
    let mut ancestor = Some(build_dir);
    while let Some(dir) = ancestor {
        if dir.components().count() < 3 {
            break;
        }
        if let Some(dir) = dir.to_str() {
            cmdline = cmdline.replace(&format!("{dir}/"), &"../".repeat(ups));
        }
        ups += 1;
        ancestor = dir.parent();
    }
    cmdline
}

/// First occurrence of each path, order preserved.
fn dedup_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if seen.insert(p.clone()) {
            out.push(p.clone());
        }
    }
    out
}

/// The `include/<Module>` root shared by a rule's declared outputs, when at
/// least two of them agree on it. Returns None when the rule declares no
/// include-tree outputs or when they disagree - a single output would make
/// its own parent the "common" root, which for `include/QtSvg/QtSvgDepends`
/// is right and for anything else is a guess, so two is the floor.
fn common_include_root(outputs: &[PathBuf]) -> Option<PathBuf> {
    // CMAKE AUTOGEN IS THE SECOND WRITER OF AN UNDECLARED TREE. Its edge
    // declares `<T>_autogen/mocs_compilation.cpp` and a timestamp, and moc
    // writes every `<name>.moc` a source `#include`s into
    // `<T>_autogen/include/` - declared nowhere, consumed by the compile of
    // that source (qtsvg's imageformats plugin, `main.moc: No such file`,
    // 2026-08-23). Same remedy as syncqt: the directory rides the declared
    // outputs as a co-output, and is_tree_path() recognises it downstream.
    if let Some(r) = autogen_include_root(outputs) {
        return Some(r);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for o in outputs {
        let mut comps = o.components();
        if comps.next()?.as_os_str() != "include" {
            continue;
        }
        if let Some(module) = comps.next() {
            let r = Path::new("include").join(module.as_os_str());
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
    }
    // Exactly one module tree, named by at least two declared outputs.
    if roots.len() != 1 {
        return None;
    }
    let root = roots.remove(0);
    // ONE NAMED OUTPUT IS THE FLOOR, NOT TWO, and the two-output version is
    // why this shipped inert. The rule it has to cover is syncqt, whose edge
    // declares a timestamp and a SINGLE include file - so a floor of two
    // rejected the one task that writes the tree, while firing happily on a
    // neighbouring rule that declares several. Measured: exactly one tree
    // output in a whole qtsvg run, and not from the syncqt task, which is
    // indistinguishable from the tree working until you check WHICH task
    // produced it.
    // A task with one include output now declares a directory holding that
    // file, which is redundant and correct - the same argument that lets
    // this be structural rather than gated on the command.
    let named = outputs.iter().filter(|o| o.starts_with(&root)).count();
    if named >= 1 {
        Some(root)
    } else {
        None
    }
}

/// `<dir>/<T>_autogen` when every declared output sits directly under one
/// `<T>_autogen` component; None otherwise. One root, like the syncqt rule,
/// because two targets' autogen dirs in one edge is not a shape CMake emits.
fn autogen_include_root(outputs: &[PathBuf]) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for o in outputs {
        // ONLY THE AUTOGEN EDGE ITSELF, recognised by what it declares:
        // `<T>_autogen/mocs_compilation.cpp` or `<T>_autogen/timestamp`.
        // The object that COMPILES mocs_compilation.cpp lives at
        // `CMakeFiles/<T>.dir/<T>_autogen/mocs_compilation.cpp.o`, which
        // also contains an `_autogen` component, and the first version of
        // this matched it and declared a tree that no task writes -
        // `canonicalize(.../<T>.dir/<T>_autogen/include): No such file`.
        let Some(parent) = o.parent() else { continue };
        let file = o.file_name().map(|f| f.to_string_lossy().into_owned());
        if !matches!(
            file.as_deref(),
            Some("mocs_compilation.cpp") | Some("timestamp")
        ) {
            continue;
        }
        if !parent
            .file_name()
            .is_some_and(|d| d.to_string_lossy().ends_with("_autogen"))
        {
            continue;
        }
        let r = parent.to_path_buf();
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    if roots.len() != 1 {
        return None;
    }
    // THE WHOLE `<T>_autogen` DIRECTORY, not only its include/ half: moc
    // writes `<T>_autogen/<HASH>/moc_x.cpp` and mocs_compilation.cpp
    // `#include`s them by that relative path (SvgWidgets: "EWIEGA46WW/
    // moc_qsvgwidget.cpp: No such file", 2026-08-23). The declared outputs
    // sit inside it too, which is the redundant-and-correct case. The graph
    // node named `<T>_autogen` is CMake's phony; add_derived_file keys the
    // tree apart from it.
    Some(roots.remove(0))
}

/// Every undeclared directory an edge is known to write, from its declared
/// outputs: syncqt's `include/<Module>`, and for CMake's autogen edge both
/// `<T>_autogen` and `CMakeFiles/<T>_autogen.dir`. ONE function for the two
/// places that need the list - new_task, which declares them as task
/// outputs, and the build-result path in start(), which records what the
/// task produced. They computed it separately until 2026-08-23, the second
/// knew only the first tree, and the AUTOMOC-extraction edge then opened
/// `CMakeFiles/Svg_autogen.dir/ParseCache.txt` that the task had written
/// and the driver had never recorded.
fn undeclared_trees(outputs: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = common_include_root(outputs) {
        v.push(d);
    }
    // THE AUTOGEN EDGE WRITES A SECOND UNDECLARED DIRECTORY:
    // `CMakeFiles/<T>_autogen.dir/` holds ParseCache.txt, which the later
    // AUTOMOC-extraction edge for the same target opens by absolute path
    // ("Could not open: .../CMakeFiles/Svg_autogen.dir/ParseCache.txt",
    // qtsvg, 2026-08-23). Its AutogenInfo.json is a configure-time input
    // living in the same directory, which link_tree leaves in place.
    if let Some(d) = autogen_cache_dir(outputs) {
        if !v.contains(&d) {
            v.push(d);
        }
    }
    v
}

/// Every output an edge is known to write and ninja does not declare: the
/// directory trees, plus Qt's FINAL .prl FILE. QtPrlHelpers.cmake says in
/// as many words that the final prl "should not be specified as a
/// BYPRODUCT": the step2 edge's script writes it as a side effect, to the
/// path recorded in the edge's own `-DIN_META_FILE` as
/// `FINAL_PRL_FILE_PATH = <abs>`, and `qt_install(FILES)` then installs it
/// ("file INSTALL cannot find .../lib/libQt6Svg.prl", 2026-08-23). The
/// meta file is configure-time content in the build dir, so it is readable
/// at planning. One function for both the planning and the result paths.
fn undeclared_outputs(
    declared: &[PathBuf],
    cmdline: Option<&str>,
    build_dir: &Path,
) -> Vec<PathBuf> {
    let mut v = undeclared_trees(declared);
    if let Some(c) = cmdline {
        if c.contains("QtFinishPrlFile.cmake") {
            if let Some(meta) = c
                .split_whitespace()
                .find_map(|t| t.strip_prefix("-DIN_META_FILE="))
            {
                let meta = meta.trim_matches('"');
                let meta_path = if Path::new(meta).is_absolute() {
                    PathBuf::from(meta)
                } else {
                    build_dir.join(meta)
                };
                if let Ok(text) = fs::read_to_string(&meta_path) {
                    for line in text.lines() {
                        if let Some(p) = line.strip_prefix("FINAL_PRL_FILE_PATH = ") {
                            let p = p.trim();
                            let rel = relative_from(Path::new(p), build_dir)
                                .unwrap_or_else(|| PathBuf::from(p));
                            if !v.contains(&rel) {
                                v.push(rel);
                            }
                        }
                    }
                }
            }
        }
        // `cmake -E create_symlink <target> <link>`: cmake's custom-target
        // symlink shape (lz4cat, unlz4). The edge declares only the
        // CMakeFiles stamp; the link itself is an undeclared side effect
        // that real ninja leaves in the build tree and a sandboxed task
        // discards. The link is relative to the command's `cd <dir> &&`
        // prefix when one is present, the build dir otherwise.
        let toks: Vec<&str> = c.split_whitespace().collect();
        for w in toks.windows(4) {
            if w[0] == "-E" && w[1] == "create_symlink" {
                let link = w[3].trim_matches('"');
                let base = if toks.first() == Some(&"cd") && toks.get(2) == Some(&"&&") {
                    PathBuf::from(toks[1].trim_matches('"'))
                } else {
                    build_dir.to_path_buf()
                };
                let abs = if Path::new(link).is_absolute() {
                    PathBuf::from(link)
                } else {
                    base.join(link)
                };
                let rel = relative_from(&abs, build_dir).unwrap_or_else(|| PathBuf::from(link));
                if !v.contains(&rel) {
                    v.push(rel);
                }
            }
        }
    }
    v
}

/// `<dir>/CMakeFiles/<T>_autogen.dir` for an autogen edge whose outputs
/// sit under `<dir>/<T>_autogen`; None for any other edge.
fn autogen_cache_dir(outputs: &[PathBuf]) -> Option<PathBuf> {
    let root = autogen_include_root(outputs)?;
    let t = root.file_name()?.to_string_lossy().into_owned();
    Some(root.parent()?.join("CMakeFiles").join(format!("{t}.dir")))
}

/// The shape wait() attaches as a co-output and the gcc header filter
/// must let through: a syncqt module tree (`include/<Module>`) or a CMake
/// autogen dir (`.../<T>_autogen`). One predicate, so the
/// three sites that need it cannot drift.
fn is_tree_path(p: &Path) -> bool {
    // The lookup-key spelling from add_derived_file.
    let p = match p.to_str().and_then(|s| s.strip_suffix("/.nn-tree")) {
        Some(stripped) => Path::new(stripped),
        None => p,
    };
    let comps: Vec<_> = p.components().collect();
    // A syncqt module tree is `include/<Module>` where Module is a
    // DIRECTORY with no extension (include/QtSvg). A plain generated
    // FILE directly under include/ matches the same two-component shape:
    // meson's vcs_tag writes include/vcs_version.h, this predicate
    // classified it as a tree, add_derived_file keyed it under the
    // `/.nn-tree` marker, and the graph node's consumers missed it in
    // derived_files - so every compile order-only-depending on it fell
    // through to a disk upload of a file that never exists outside the
    // producer's sandbox (libvmaf, ninth class, 2026-08-23). The
    // extension is the discriminator: Qt module dirs have none.
    if comps.len() == 2 && p.starts_with("include") && p.extension().is_none() {
        return true;
    }
    p.file_name().is_some_and(|f| {
        let f = f.to_string_lossy();
        f.ends_with("_autogen") || f.ends_with("_autogen.dir")
    })
}

/// Classify one build-dir symlink for the alias carry. Accepts only a
/// link whose TARGET TEXT is relative and stays inside the build dir
/// lexically (each `..` must not climb past the link's own directory),
/// and whose link path and target both survive the `link=target`
/// space-separated encoding. Everything else is skipped, and skipping is
/// the safe direction: the pre-fix behavior for every symlink.
fn alias_symlink_entry(build_dir: &Path, link_abs: &Path) -> Option<(String, String)> {
    let target = std::fs::read_link(link_abs).ok()?;
    if target.is_absolute() {
        return None;
    }
    let rel_link = link_abs.strip_prefix(build_dir).ok()?;
    // Lexical confinement: resolve `..` against the link's directory
    // depth. A target climbing out of the build dir is not an alias to a
    // build product and must not be recreated in a sandbox.
    let link_depth = rel_link.components().count().saturating_sub(1);
    let mut depth = link_depth as isize;
    for c in target.components() {
        match c {
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    let l = rel_link.to_str()?;
    let t = target.to_str()?;
    if l.contains([' ', '=']) || t.contains([' ', '=']) {
        return None;
    }
    Some((l.to_string(), t.to_string()))
}

fn leading_parent_components(p: &Path) -> usize {
    p.components()
        .take_while(|c| matches!(c, std::path::Component::ParentDir))
        .count()
}

/// Join a relative path onto a base LEXICALLY: each `..` pops a base
/// component instead of surviving into the result, so
/// gen/ui/webui/resources/js + ../../../../../../../../../../src/x
/// becomes ../../../../../src/x against the build dir - resolvable by
/// the existing five-up sandbox mirror, where the raw twenty-component
/// climb is not. Pops past the base accumulate as leading `..`.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    let mut stack: Vec<std::path::Component> = base.components().collect();
    for c in rel.components() {
        match c {
            std::path::Component::ParentDir => {
                if matches!(stack.last(), Some(std::path::Component::Normal(_))) {
                    stack.pop();
                } else {
                    stack.push(c);
                }
            }
            std::path::Component::CurDir => {}
            other => stack.push(other),
        }
    }
    stack.iter().map(|c| c.as_os_str()).collect()
}

/// Upload one referenced source file, plus - for a python script - its
/// same-directory .py siblings (gcc_link_wrapper.py imports
/// wrapper_utils.py; python resolves sibling imports from the script's
/// own directory). Returns every DerivedFile created, main file first.
/// This is THE upload path for referenced files: the ordering-ins loop,
/// the cmdline scan's node branch and its non-node branch all route
/// here, because the sibling rule was first added to only two of the
/// three and the third is where GN's declared `| script` inputs go.
fn upload_referenced_file(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    path: PathBuf,
) -> Result<Vec<DerivedFile>> {
    let is_py = path.extension().is_some_and(|e| e == "py");
    let main = new_opaque_file(rpc_client, build_dir, path.clone())?;
    let mut out = vec![main];
    if is_py {
        if let Some(dir) = path.parent() {
            upload_python_closure(rpc_client, build_dir, dir, &mut out)?;
        }
    }
    Ok(out)
}

/// Python module names shipped with the interpreter: an import of one
/// is satisfied by the runtime, so it must never trigger the ancestor
/// and vendored-tree probes below. Incomplete by design - a missed name
/// costs a few stats and a failed directory probe, never a wrong file.
const PY_STDLIB: &[&str] = &[
    "abc",
    "argparse",
    "ast",
    "base64",
    "binascii",
    "bisect",
    "codecs",
    "collections",
    "contextlib",
    "copy",
    "csv",
    "ctypes",
    "dataclasses",
    "datetime",
    "difflib",
    "enum",
    "errno",
    "fnmatch",
    "functools",
    "getopt",
    "glob",
    "gzip",
    "hashlib",
    "heapq",
    "html",
    "http",
    "io",
    "importlib",
    "inspect",
    "itertools",
    "json",
    "keyword",
    "locale",
    "logging",
    "math",
    "multiprocessing",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pickle",
    "platform",
    "posixpath",
    "pprint",
    "queue",
    "random",
    "re",
    "shlex",
    "shutil",
    "signal",
    "site",
    "socket",
    "stat",
    "string",
    "struct",
    "subprocess",
    "sys",
    "tempfile",
    "textwrap",
    "threading",
    "time",
    "traceback",
    "types",
    "typing",
    "unittest",
    "urllib",
    "uuid",
    "warnings",
    "xml",
    "zipfile",
    "zlib",
];

/// Upload the TRANSITIVE python dependency closure rooted at a script's
/// directory. Chromium's build scripts resolve imports through every
/// mechanism python has - sibling modules, sibling packages, uncle
/// directories whose names need not match the module, sys.path splices
/// assembled across statements, and vendored version-suffixed trees
/// (beautifulsoup4-4.9.3/py3k/bs4) - and they do it TRANSITIVELY:
/// parse_html_deps.py, itself two packages deep, imports bs4. So every
/// uploaded package directory gets the same scan its referencing script
/// got, until the closure is dry. Bounded by a directory-count cap and
/// a visited set; existence discriminates every candidate.
fn upload_python_closure(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
    out: &mut Vec<DerivedFile>,
) -> Result<()> {
    // Memoized on the script DIRECTORY alone. The key was
    // (script dir, script) and that coarseness was the whole serial
    // bottleneck at Chromium scale: the walk depends on the script only
    // through a skip of the script's own file, so every script in a
    // shared directory (mojom, grit: hundreds per dir) re-paid the full
    // tree walk under a fresh key. perf on round 60: Path::join plus
    // malloc churn under upload_python_closure_uncached at ~50% of all
    // driver cycles. The skip is gone (the script re-uploads as one
    // more content-cached file and the consumer's input_set dedupes by
    // path), so the result is script-independent and one walk per
    // directory serves every script in it.
    out.extend(
        python_closure_cached(rpc_client, build_dir, start_dir)?
            .iter()
            .cloned(),
    );
    Ok(())
}

/// The memoized closure behind upload_python_closure, shared by Arc so
/// a caller that only needs to CHECK membership (the per-task python
/// sibling pass) iterates the one canonical copy instead of cloning
/// thousands of DerivedFiles per hit - the clone-on-hit was 15% of
/// driver cycles (malloc/free) on round 65's profile.
fn python_closure_cached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
) -> Result<Arc<Vec<DerivedFile>>> {
    static CLOSURE_MEMO: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, Arc<Vec<DerivedFile>>>>,
    > = std::sync::OnceLock::new();
    let memo = CLOSURE_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(start_dir) {
        return Ok(hit.clone());
    }
    // Cross-round: a validated entry from the persisted cache skips the
    // walk and every add_to_store_nar round trip; see resolve_cache.rs.
    if let Some(files) = crate::resolve_cache::lookup("py", start_dir) {
        let arc = Arc::new(files);
        memo.lock()
            .unwrap()
            .insert(start_dir.to_path_buf(), arc.clone());
        return Ok(arc);
    }
    let mut fresh: Vec<DerivedFile> = Vec::new();
    upload_python_closure_uncached(rpc_client, build_dir, start_dir, &mut fresh)?;
    crate::resolve_cache::record("py", start_dir, &fresh);
    let arc = Arc::new(fresh);
    memo.lock()
        .unwrap()
        .insert(start_dir.to_path_buf(), arc.clone());
    Ok(arc)
}

fn upload_python_closure_uncached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
    out: &mut Vec<DerivedFile>,
) -> Result<()> {
    const CLOSURE_DIR_CAP: usize = 64;
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut queue: Vec<PathBuf> = vec![start_dir.to_path_buf()];
    let upload_dir = |dir: &Path, cap: usize, out: &mut Vec<DerivedFile>| -> Result<bool> {
        match walk_dir_capped(rpc_client, build_dir, dir, cap)? {
            Some(files) => {
                out.extend(files);
                Ok(true)
            }
            None => {
                println!(
                    "nix-ninja: python dep dir {} exceeds {} files; skipped",
                    dir.display(),
                    cap
                );
                Ok(false)
            }
        }
    };
    while let Some(dir) = queue.pop() {
        if !visited.insert(dir.clone()) || visited.len() > CLOSURE_DIR_CAP {
            continue;
        }
        // Siblings: every .py module, node_modules for node.py, and
        // package directories - which recurse, because their own files
        // import too.
        let entries = fs::read_dir(&dir)
            .map_err(|e| anyhow!("read_dir({}) for python closure: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "py") && p.is_file() {
                out.push(new_opaque_file(rpc_client, build_dir, p)?);
            } else if p.is_dir() && p.file_name().is_some_and(|n| n == "node_modules") {
                upload_dir(&p, 8192, out)?;
            } else if p.is_dir() && p.join("__init__.py").is_file() && upload_dir(&p, 512, out)? {
                queue.push(p);
            }
        }
        // Imports this directory's files make that nothing here satisfies.
        let unsatisfied: Vec<String> = python_import_names(&dir)?
            .into_iter()
            .filter(|name| {
                !PY_STDLIB.contains(&name.as_str())
                    && !dir.join(format!("{name}.py")).is_file()
                    && !dir.join(name).is_dir()
            })
            .collect();
        if let Some(parent) = dir.parent() {
            if !unsatisfied.is_empty() {
                // Uncles: a directory beside this one holding <name>.py
                // (common/ holds models.py) or BEING the named package
                // (tracing/tracing_build/ answers import tracing_build).
                for uncle in fs::read_dir(parent)
                    .map_err(|e| anyhow!("read_dir({}) for uncle modules: {e}", parent.display()))?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && *p != dir)
                {
                    let hit = unsatisfied.iter().any(|n| {
                        uncle.join(format!("{n}.py")).is_file()
                            || (uncle.file_name().is_some_and(|f| f == n.as_str())
                                && uncle.join("__init__.py").is_file())
                    });
                    if hit && upload_dir(&uncle, 512, out)? {
                        queue.push(uncle);
                    }
                }
                // Ancestors, by chromium's layout: <root>/<name>/,
                // <root>/third_party/<name>/, and the vendored deep
                // shape <root>/third_party/<pkg-x.y.z>[/<subdir>]/<name>/
                // (beautifulsoup4-4.9.3/py3k/bs4). Four levels up, first
                // hit per name; the deep scan runs only when the cheap
                // probes miss, and only over third_party.
                for name in &unsatisfied {
                    let mut anc = parent.to_path_buf();
                    'levels: for _ in 0..4 {
                        let Some(up) = anc.parent() else { break };
                        anc = up.to_path_buf();
                        for cand in [anc.join(name), anc.join("third_party").join(name)] {
                            let module = cand.join(format!("{name}.py")).is_file();
                            let package = cand.join("__init__.py").is_file();
                            if module || package {
                                if upload_dir(&cand, 8192, out)? && package {
                                    queue.push(cand);
                                }
                                break 'levels;
                            }
                        }
                        // A vendored tree can hold SEVERAL copies of one
                        // package - catapult carries a legacy
                        // beautifulsoup4/ AND beautifulsoup4-4.9.3/py3k/,
                        // and its own sys.path assembly picks the py3 one
                        // across three statements no scan can chase. So
                        // collect every candidate and prefer the
                        // python-3, version-suffixed copy: the legacy one
                        // imports interfaces that no longer exist
                        // (html5lib.treebuilders._base, measured).
                        let tp = anc.join("third_party");
                        let mut cands: Vec<PathBuf> = Vec::new();
                        if let Ok(subs) = fs::read_dir(&tp) {
                            for sub in subs.flatten().map(|e| e.path()) {
                                if !sub.is_dir() {
                                    continue;
                                }
                                let direct = sub.join(name);
                                if direct.join("__init__.py").is_file() {
                                    cands.push(direct);
                                }
                                // No else: a direct hit must not hide a
                                // deeper sibling copy of the same package
                                // (4.9.3/bs4 sits BESIDE 4.9.3/py3k/bs4,
                                // and only the deeper one is python 3).
                                if let Ok(subs2) = fs::read_dir(&sub) {
                                    cands.extend(
                                        subs2
                                            .flatten()
                                            .map(|e| e.path().join(name))
                                            .filter(|d| d.join("__init__.py").is_file()),
                                    );
                                }
                            }
                        }
                        let score = |p: &Path| -> u32 {
                            let s = p.to_string_lossy().into_owned();
                            let versioned = s.split('/').any(|c| {
                                c.contains('-') && c.chars().any(|ch| ch.is_ascii_digit())
                            });
                            u32::from(versioned) * 2 + u32::from(s.contains("py3"))
                        };
                        if let Some(pkg) = cands.into_iter().max_by_key(|p| score(p)) {
                            if upload_dir(&pkg, 8192, out)? {
                                queue.push(pkg);
                            }
                            break 'levels;
                        }
                    }
                }
                // Descendants: a sys.path.insert into the importer's OWN
                // subtree, carried through a variable the one-line splice
                // reader cannot chase (json_schema_compiler inserts
                // ppapi/generators and imports idl_parser from it; the
                // ppapi dir has no __init__.py, so package recursion never
                // descends there). Bounded walk for <name>.py or
                // <name>/__init__.py; the CONTAINING dir uploads and
                // queues so its own imports (idl_lexer -> ply) get chased.
                for name in &unsatisfied {
                    if let Some(holder) = find_module_below(&dir, name, 3) {
                        if upload_dir(&holder, 512, out)? {
                            queue.push(holder);
                        }
                    }
                }
            }
            // sys.path splices readable on one line still help for the
            // shapes the structural probes miss.
            for sp in python_syspath_dirs(&dir)? {
                if upload_dir(&sp, 8192, out)? {
                    queue.push(sp);
                }
            }
            // A *_project.py file is catapult's convention for a
            // vulcanize project definition: its os.path.join lines name
            // the DATA roots the tool searches (tracing/, polymer
            // components), assembled from attributes no import scan can
            // see. Each join line's quoted segments resolve against the
            // file's own dir, its parent, and the parent's third_party;
            // existing directories upload, same bound as everything
            // here.
            let has_project_file = fs::read_dir(&dir)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with("_project.py"));
            if has_project_file {
                let mut bases = vec![dir.clone()];
                if let Some(par) = dir.parent() {
                    bases.push(par.to_path_buf());
                    bases.push(par.join("third_party"));
                }
                for seg_path in python_join_segments(&dir)? {
                    for base in &bases {
                        let cand = lexical_join(base, &seg_path);
                        if cand.is_dir() && cand != dir {
                            if upload_dir(&cand, 8192, out)? {
                                queue.push(cand);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Every file="..."/path="..." value of a grit manifest resolved
/// lexically against the manifest's directory, EXISTENCE-BLIND: the
/// caller matches these against graph nodes to catch generated
/// references, which exist nowhere on disk at scan time.
fn grd_reference_candidates(grd: &Path) -> Result<Vec<PathBuf>> {
    let body = fs::read_to_string(grd)
        .map_err(|e| anyhow!("read({}) for grd candidates: {e}", grd.display()))?;
    let dir = grd.parent().unwrap_or(Path::new(""));
    let mut out = Vec::new();
    for attr in ["file=\"", "path=\""] {
        for chunk in body.split(attr).skip(1) {
            let Some(val) = chunk.split('"').next() else {
                continue;
            };
            if val.is_empty() || val.contains("://") {
                continue;
            }
            out.push(lexical_join(dir, Path::new(val)));
        }
    }
    Ok(out)
}

// Source files a generate_grd.py invocation names on its cmdline:
// `--input-files f1 f2 ...` relative to `--input-files-base-dir base`,
// where base is relative to the repo root. The root is derived from the
// tool's own path token (it always ends `ui/webui/resources/tools/`
// `generate_grd.py`), so the function is tree-layout-independent.
// Existence-filtered: a name that resolves nowhere is a manifest entry
// for a GENERATED file, which the edge's declared inputs already carry.
// (These lines sit AFTER the function they describe, so they are comments
// rather than doc comments - as `///` they documented the statics below.)
//
// Section buckets for new_task's serial-resolution cost, printed by the
// heartbeat in start(): worklist expansion, cmdline scan (incl. the
// implicit-inputs blanket), the py sibling post-pass, and the grd pass.

/// The driver's own resident size, in MiB, or 0 where /proc is unavailable.
///
/// The 500-task tick reported counts and four timers and nothing about the
/// driver's own footprint, so a climb from 0 to 13.19 GiB across one round's
/// resolution - and the drop back to 2.10 GiB the instant resolution ended -
/// was found by reading /proc by hand, after the round that needed it. The
/// footprint is resolve-phase state rather than a leak, which is a fact worth
/// having in the log rather than in somebody's shell history.
///
/// statm field 2 is resident pages. A failed read reports 0: this is a
/// progress line, and a driver that cannot read its own /proc must not die
/// mid-round over a diagnostic.
fn self_rss_mib() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| pages * 4096 / (1024 * 1024))
        .unwrap_or(0)
}

/// Live heap against allocator-retained heap, the pair RSS cannot separate.
///
/// Returns `(in_use, retained)` in MiB. RSS is `retained` plus everything that
/// is not heap, so RSS alone cannot tell a growing working set from an
/// allocator sitting on its high-water mark - and three rounds of RSS-only
/// ticks did not tell them apart. The driver runs 26 threads against glibc's
/// default of up to `8 * nproc` arenas (192 here), which is exactly the
/// arrangement where the two readings diverge.
///
/// The discriminator: `retained` climbing with `in_use` flat is retention, and
/// `MALLOC_ARENA_MAX` is the fix. Both climbing together is live state, and
/// the fix is in the resolve-phase data structures. Their difference is
/// `fordblks`, the free-but-held bytes, which is the bloat itself.
///
/// WHICH FIELDS, measured rather than assumed, because the obvious reading of
/// the man page is wrong in the direction that reads as a measurement:
///
/// - `in_use` is `uordblks + hblkhd`, NOT `uordblks`. Anything past the mmap
///   threshold bypasses the arena, so a 128 MiB allocation lands entirely in
///   `hblkhd` and leaves `uordblks` unmoved. A tick reporting `uordblks` alone
///   printed `0 -> 0 MiB` across a live 128 MiB vector; the first version of
///   this function did exactly that and its own test caught it.
/// - `arena` sums the NON-MAIN arenas too, verified with an 8-thread probe
///   (268 MiB held, 1.2 MiB after the frees). Had it been main-arena-only,
///   this tick would have been blind to precisely the per-thread retention it
///   exists to measure, while still printing a plausible number.
/// - mmapped bytes are returned to the kernel on free, so they are never
///   retention; they appear in both terms and cancel out of the difference.
///
/// mallinfo2 rather than mallinfo: mallinfo's fields are `int`, and this
/// driver has been measured at 13.19 GiB, so every field of the older call
/// would have overflowed and reported a plausible small number - a wrong
/// reading that announces nothing, which is the class this project keeps
/// paying for.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn self_heap_mib() -> Option<(u64, u64)> {
    // SAFETY: mallinfo2 takes no arguments, reads the allocator's own
    // counters, and returns a plain struct of integers. It walks every arena
    // holding the malloc lock, so it is not free: called once per resolve
    // tick, never per task.
    let mi = unsafe { libc::mallinfo2() };
    const MIB: u64 = 1024 * 1024;
    Some((
        (mi.uordblks as u64 + mi.hblkhd as u64) / MIB,
        (mi.arena as u64 + mi.hblkhd as u64) / MIB,
    ))
}

/// mallinfo2 is glibc's. Everywhere else the tick omits the pair rather than
/// printing zeros: a zero here is indistinguishable from an allocator holding
/// nothing, and this line exists to be read as a measurement.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn self_heap_mib() -> Option<(u64, u64)> {
    None
}

/// Wall time inside `handle_derivation_result`, the dynamic-dependency
/// discovery that the resolve tick never counted. See the note at the top of
/// that function for what the omission cost.
/// Declared header-shaped inputs offered to discovery, and the subset the
/// preprocessor was observed to open. Their ratio is used-input pruning's
/// whole case, and it had never been measured on this build.
///
/// THE TWO SITES ARE NOT INTERCHANGEABLE and the first version of this got it
/// wrong. `discovered_deps` is what the compile read AND was not already
/// declared - discover_c_includes drops the overlap - so comparing it against
/// the declared set compares two sets that cannot intersect, and would have
/// printed 100% prunable on any build. The numerator is therefore counted
/// inside that filter's TAKEN branch, where membership in both sets is the
/// branch condition and disjointness is impossible by construction.
///
/// THE SEAM THAT WOULD REOPEN THIS, named because it is not a defect today
/// and so has nothing else to mark it. The numerator tests `header_like` on
/// `include`; the denominator tests it on `input.build_path`. Those are one
/// population only while `built_paths` is keyed by BUILD path. Key it by
/// store path in some later refactor and the numerator silently returns to
/// zero while the denominator stays nonzero - the original defect, arriving
/// through a change that has nothing to do with this counter.
///
/// Which is why `prune_line`'s refusal is a standing detector rather than
/// scaffolding for a fixed bug: do not retire it once the ratio starts
/// looking believable. Raised by the specification session, e8d6b02.
///
/// Counted whether or not pruning is enabled, so the next round anybody runs
/// prices the change with no behavior change and no risk.
static DECLARED_HEADERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static USED_HEADERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A zero numerator against a nonzero denominator is the SIGNATURE of the
/// disjointness defect this counter shipped with once - two sets that cannot
/// intersect - and not a finding of 100% dead weight. It fails loud rather
/// than quiet, which is worse: a spectacular number is the one that gets
/// quoted. Refuse to print a ratio on it and say why.
fn prune_line(declared: u64, kept: u64) -> String {
    if declared == 0 {
        String::new()
    } else if kept == 0 {
        ", hdrs SUPPRESSED (0 used / nonzero declared - disjoint sets?)".to_string()
    } else {
        format!(
            ", hdrs {kept} used / {declared} declared ({} prunable)",
            declared.saturating_sub(kept)
        )
    }
}

/// One definition, because the numerator and the denominator are counted in
/// different functions and a header-shape test that disagreed between them
/// would move the ratio with nothing to show for it.
fn header_like(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("h" | "hpp" | "hh" | "inc" | "ipp")
    )
}

/// dyn's two expensive halves, separated because they have different fixes.
static DYN_UPDATE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// CALL COUNTS, not just totals, and the reason is a question the totals
// cannot answer. Both calls fire once per DYNAMIC task rather than once per
// task, so dividing either total by n_tasks understates it by whatever
// fraction of the graph is dynamic. With the count, adddrv per call is the
// discriminator: flat per call means a fixed round-trip cost and a batch
// fixes it, while growth with task count means it is carrying the same
// superlinear input set that realise-asked shows, and a memo alone will not
// bound it. Raised by the specification session, addendum 733.
static DYN_UPDATE_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DYN_ADDDRV_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// The same add_drv_to_store call on the non-dynamic arm. Separate counters
// because the two arms have separate populations: pooling a call that runs on
// every task with one that runs only on dynamic tasks reports neither.
static DYN_PLAIN_ADDDRV_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DYN_PLAIN_ADDDRV_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DYN_ADDDRV_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DYN_REALISE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DYN_DISCOVER_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static DYN_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct DynDiscoveryTimer(std::time::Instant);

impl Drop for DynDiscoveryTimer {
    fn drop(&mut self) {
        DYN_MS.fetch_add(
            self.0.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

static NT_WORKLIST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NT_CMDLINE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NT_PY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NT_GRD_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn generate_grd_input_files(cmdline: &str) -> Vec<PathBuf> {
    const TOOL_SUFFIX: &str = "ui/webui/resources/tools/generate_grd.py";
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    let Some(tool) = toks.iter().find(|t| t.ends_with(TOOL_SUFFIX)) else {
        return Vec::new();
    };
    let root = &tool[..tool.len() - TOOL_SUFFIX.len()];
    let mut base: Option<&str> = None;
    let mut files: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "--input-files-base-dir" => {
                base = toks.get(i + 1).copied();
                i += 2;
            }
            "--input-files" => {
                i += 1;
                while i < toks.len() && !toks[i].starts_with("--") {
                    files.push(toks[i]);
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    let Some(base) = base else {
        return Vec::new();
    };
    files
        .into_iter()
        .map(|f| PathBuf::from(format!("{root}{base}/{f}")))
        .filter(|p| p.is_file())
        .collect()
}

/// Quoted string segments of every os.path.join line in the *_project.py
/// files of `dir`, each line yielding one relative path. Used only for
/// catapult-style project definition files, whose join lines name data
/// roots; the caller resolves against candidate bases and keeps what
/// exists.
fn python_join_segments(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow!("read_dir({}) for project scan: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with("_project.py"))
        {
            continue;
        }
        let Ok(body) = fs::read_to_string(&p) else {
            continue;
        };
        for line in body.lines() {
            if !line.contains("os.path.join") {
                continue;
            }
            let mut segs: Vec<&str> = Vec::new();
            let mut rest = line;
            while let Some(start) = rest.find(['\'', '"']) {
                let quote = rest.as_bytes()[start] as char;
                let after = &rest[start + 1..];
                let Some(end) = after.find(quote) else { break };
                let seg = &after[..end];
                if !seg.is_empty() && !seg.contains(':') {
                    segs.push(seg);
                }
                rest = &after[end + 1..];
            }
            if !segs.is_empty() {
                let rel: PathBuf = segs.iter().collect();
                if !found.contains(&rel) {
                    found.push(rel);
                }
            }
        }
    }
    Ok(found)
}

/// The directory below `root` holding `<name>.py`, or the package dir
/// `<name>/` holding an `__init__.py` - whichever a runtime
/// sys.path.insert into the importer's own subtree would reach. BFS to
/// `depth`, skipping hidden dirs and node_modules; first hit wins.
fn find_module_below(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for d in frontier {
            let entries = fs::read_dir(&d).ok()?;
            for sub in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                let fname = sub.file_name().unwrap_or_default().to_string_lossy();
                if fname.starts_with('.') || fname == "node_modules" {
                    continue;
                }
                if sub.join(format!("{name}.py")).is_file() {
                    return Some(sub);
                }
                if fname == name && sub.join("__init__.py").is_file() {
                    return Some(sub);
                }
                next.push(sub);
            }
        }
        frontier = next;
    }
    None
}

/// Directories the .py files in `dir` splice onto sys.path, read
/// textually: every line containing "sys.path" contributes its quoted
/// string segments, joined in order and resolved lexically against the
/// script directory. Existence-discriminated by the caller; a segment
/// list that is not a real relative path simply resolves to nothing.
fn python_syspath_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow!("read_dir({}) for sys.path scan: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "py") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&p) else {
            continue;
        };
        for line in body.lines() {
            if !line.contains("sys.path") {
                continue;
            }
            let mut segs: Vec<&str> = Vec::new();
            let mut rest = line;
            while let Some(start) = rest.find(['\'', '"']) {
                let quote = rest.as_bytes()[start] as char;
                let after = &rest[start + 1..];
                let Some(end) = after.find(quote) else { break };
                let seg = &after[..end];
                if !seg.is_empty() && !seg.contains(':') {
                    segs.push(seg);
                }
                rest = &after[end + 1..];
            }
            if segs.is_empty() {
                continue;
            }
            let rel: PathBuf = segs.iter().collect();
            let cand = lexical_join(dir, &rel);
            if cand.is_dir() && cand != dir && !found.contains(&cand) {
                found.push(cand);
            }
        }
    }
    Ok(found)
}

/// Upload every regular file under a directory the command names as an
/// argument (recursive), capped so a mistaken match cannot ingest a
/// source tree: past the cap the walk STOPS AND FAILS rather than
/// silently truncating - a partial package import fails stranger than a
/// named refusal. 512 covers dawn's jinja2 (~60 files incl. templates);
/// raise it deliberately if a real consumer needs more.
fn upload_referenced_dir(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
) -> Result<Vec<DerivedFile>> {
    // Memoized by directory, same reasoning as CLOSURE_MEMO above: the
    // result depends only on the directory's contents, and at Chromium
    // scale one dir arg (the chromium root, passed by thousands of
    // inspector_protocol-style edges) was re-walked 2,093 times in one
    // round, 327 files per walk, dominating the py resolve bucket after
    // the closure memo landed (round 63: 100s cumulative at task 8,000,
    // 397s at 9,000 - the marginal cost was this walk).
    static DIR_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Vec<DerivedFile>>>> =
        std::sync::OnceLock::new();
    let memo = DIR_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(dir) {
        return Ok(hit.clone());
    }
    // Cross-round persistence, same shape as python_closure_cached.
    if let Some(files) = crate::resolve_cache::lookup("dir", dir) {
        memo.lock()
            .unwrap()
            .insert(dir.to_path_buf(), files.clone());
        return Ok(files);
    }
    let fresh = upload_referenced_dir_uncached(rpc_client, build_dir, dir)?;
    crate::resolve_cache::record("dir", dir, &fresh);
    memo.lock()
        .unwrap()
        .insert(dir.to_path_buf(), fresh.clone());
    Ok(fresh)
}

fn upload_referenced_dir_uncached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
) -> Result<Vec<DerivedFile>> {
    // 1024, raised from 512 on 2026-08-20: v8/tools is an importable
    // package (has __init__.py) holding 823 files, reached through the
    // over-cap parent fallback below, and the round-66 hard error asked
    // for exactly this deliberate raise. 512 covered dawn's jinja2
    // (~60 files); the cap still refuses a mistaken source-tree match.
    const DIR_UPLOAD_CAP: usize = 1024;
    match walk_dir_capped(rpc_client, build_dir, dir, DIR_UPLOAD_CAP)? {
        Some(mut files) => {
            // A package dir uploaded wholesale may import SIBLING
            // packages the command never names: jinja2 does
            // `from markupsafe import Markup`, dawn passes only
            // --jinja2-path, and upstream finds markupsafe beside it on
            // the shared filesystem. Scan the uploaded package's .py
            // files for top-level import names and upload each sibling
            // package they resolve to, one level (markupsafe imports
            // nothing further; a deeper chain will name itself when it
            // exists).
            if dir.join("__init__.py").is_file() {
                if let Some(parent) = dir.parent() {
                    for name in python_import_names(dir)? {
                        let sib = parent.join(&name);
                        if sib != dir && sib.join("__init__.py").is_file() {
                            if let Some(more) =
                                walk_dir_capped(rpc_client, build_dir, &sib, DIR_UPLOAD_CAP)?
                            {
                                files.extend(more);
                            }
                        }
                    }
                }
            }
            Ok(files)
        }
        None => {
            // Over the cap. Two conventions share this shape: dawn passes
            // the package dir itself (small, handled above), and
            // inspector_protocol passes the package's PARENT - all of
            // chromium's third_party, 310 entries - to sys.path.insert it
            // and import jinja2. Measured: only 6 of those 310 are python
            // packages, ~50 files. So the over-cap fallback uploads just
            // the immediate subdirectories that are importable packages
            // (an __init__.py at their root), each itself capped; a
            // package that ALSO busts the cap is a hard error, because a
            // partial package import fails stranger than a named refusal.
            let mut out = Vec::new();
            let entries = fs::read_dir(dir)
                .map_err(|e| anyhow!("read_dir({}) for dir arg: {e}", dir.display()))?;
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("__init__.py").is_file() {
                    match walk_dir_capped(rpc_client, build_dir, &p, DIR_UPLOAD_CAP)? {
                        Some(files) => out.extend(files),
                        None => {
                            return Err(anyhow!(
                                "python package {} holds more than {} files; \
                                 declare its files as inputs or raise \
                                 DIR_UPLOAD_CAP deliberately",
                                p.display(),
                                DIR_UPLOAD_CAP
                            ))
                        }
                    }
                }
            }
            println!(
                "nix-ninja: dir arg {} exceeds {} files; uploaded only its \
                 python packages ({} files)",
                dir.display(),
                DIR_UPLOAD_CAP,
                out.len()
            );
            Ok(out)
        }
    }
}

/// Files a grit manifest references textually: every file="..." and
/// path="..." attribute value, resolved against the manifest's own
/// directory, kept only where the file exists on disk (grd files also
/// carry output filenames in the same attributes; existence is the
/// discriminator, and a generated input would be a graph node already).
///
/// chrome_scaled_image structures resolve their file value through a
/// per-scale context directory INSERTED between the manifest dir and the
/// value (grit's ChromeScaledImage._FindInputFile), so a bare join misses
/// every image asset. The context names come from the top-level grd's
/// <output context="..."> nodes, which this per-file scan cannot see from
/// a .grdp, so the conventional chromium scale dirs are tried as well;
/// existence keeps the false candidates out, same as the base case.
fn grd_references(grd: &Path) -> Result<Vec<PathBuf>> {
    let body = fs::read_to_string(grd)
        .map_err(|e| anyhow!("read({}) for grd scan: {e}", grd.display()))?;
    let dir = grd.parent().unwrap_or(Path::new(""));
    let mut contexts: Vec<String> = [
        "default_100_percent",
        "default_200_percent",
        "default_300_percent",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for chunk in body.split("context=\"").skip(1) {
        if let Some(val) = chunk.split('"').next() {
            if !val.is_empty() && !contexts.iter().any(|c| c == val) {
                contexts.push(val.to_string());
            }
        }
    }
    let mut out = Vec::new();
    for attr in ["file=\"", "path=\""] {
        for chunk in body.split(attr).skip(1) {
            let Some(val) = chunk.split('"').next() else {
                continue;
            };
            if val.is_empty() || val.contains("://") {
                continue;
            }
            let p = dir.join(val);
            if p.is_file() {
                out.push(p);
                continue;
            }
            for ctx in &contexts {
                let p = dir.join(ctx).join(val);
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    Ok(out)
}

/// Top-level module names imported by a python package's own files:
/// `import X` and `from X import ...`, first path segment only. Lines
/// are trimmed before matching, so INDENTED imports count too: this
/// scanner once matched column 0 only, reasoning that a try-block import
/// is optional - and chromium's json_parse.py disproved it with a
/// MANDATORY import inside try/finally whose only job is restoring
/// sys.path around it (import json_comment_eater, 65 task failures in
/// round 66). Over-collecting fails toward extra uploads, which the
/// per-dir caps bound; under-collecting fails the build.
fn python_import_names(pkg: &Path) -> Result<Vec<String>> {
    // Memoized like walk_dir_capped and for the same reason: the
    // closure runs per task and re-reads the same directories' file
    // bodies thousands of times otherwise.
    static NAMES_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Vec<String>>>> =
        std::sync::OnceLock::new();
    let memo = NAMES_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(pkg) {
        return Ok(hit.clone());
    }
    let result = python_import_names_uncached(pkg)?;
    memo.lock()
        .unwrap()
        .insert(pkg.to_path_buf(), result.clone());
    Ok(result)
}

fn python_import_names_uncached(pkg: &Path) -> Result<Vec<String>> {
    let mut names = std::collections::BTreeSet::new();
    let entries = fs::read_dir(pkg)
        .map_err(|e| anyhow!("read_dir({}) for import scan: {e}", pkg.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "py") {
            continue;
        }
        let body = fs::read_to_string(&p)
            .map_err(|e| anyhow!("read({}) for import scan: {e}", p.display()))?;
        for line in body.lines() {
            let line = line.trim_start();
            let rest = if let Some(r) = line.strip_prefix("import ") {
                r
            } else if let Some(r) = line.strip_prefix("from ") {
                r
            } else {
                continue;
            };
            let first = rest.split([' ', '.', ',']).next().unwrap_or("");
            if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                names.insert(first.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Recursive regular-file upload of one directory, or None past the cap.
fn walk_dir_capped(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
    cap: usize,
) -> Result<Option<Vec<DerivedFile>>> {
    // Memoized process-wide: the python closure runs per TASK, and at
    // Chromium scale the same directories recur thousands of times -
    // including over-cap refusals, which cost a full capped walk each
    // (the chromium root was being re-walked to 8192 entries per task,
    // measured as the driver pinning a core while the build sat idle).
    // Store dedup makes repeat uploads cheap; this makes them free, and
    // makes the NEGATIVE result free too, which is the half that
    // mattered.
    // Keyed by (directory, cap) because the cap changes which entries a walk
    // is allowed to return, so a hit under one cap is not a hit under another.
    // None is a REMEMBERED NEGATIVE - the half that mattered, per the comment
    // above - not an absent entry.
    type WalkMemo = std::sync::Mutex<HashMap<(PathBuf, usize), Option<Vec<DerivedFile>>>>;
    static WALK_MEMO: std::sync::OnceLock<WalkMemo> = std::sync::OnceLock::new();
    let memo = WALK_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key = (dir.to_path_buf(), cap);
    if let Some(hit) = memo.lock().unwrap().get(&key) {
        return Ok(hit.clone());
    }
    let result = walk_dir_capped_uncached(rpc_client, build_dir, dir, cap)?;
    memo.lock().unwrap().insert(key, result.clone());
    Ok(result)
}

fn walk_dir_capped_uncached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
    cap: usize,
) -> Result<Option<Vec<DerivedFile>>> {
    let mut paths = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            fs::read_dir(&d).map_err(|e| anyhow!("read_dir({}) for dir arg: {e}", d.display()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                if paths.len() >= cap {
                    return Ok(None);
                }
                paths.push(p);
            }
        }
    }
    // Upload only after the whole walk fits the cap, so an over-cap dir
    // costs a directory scan and zero store writes. Batched adds: a
    // node_modules tree is thousands of files at one round trip each.
    let out = new_opaque_files(rpc_client, build_dir, paths)?;
    Ok(Some(out))
}

fn normalize_build_path(build_dir: &Path, p: PathBuf) -> Result<PathBuf> {
    if p.is_relative() {
        return Ok(p);
    }
    match relative_from(&p, build_dir) {
        Some(rel) if rel.is_relative() => Ok(rel),
        _ => Err(anyhow!(
            "absolute path {} does not resolve under build dir {}; refusing to relocate it silently",
            p.display(),
            build_dir.display()
        )),
    }
}

/// rel_path IS A LOCATION INSIDE THE STORE OUTPUT AND MUST NOT CLIMB.
/// An out-of-build-dir output (openfec's ../bin/Release/libopenfec.so)
/// keeps its ../ in build_path, which is a SANDBOX location and is
/// correct there - but carried verbatim into rel_path, the producer's
/// copy target `$out/../bin/...` lands OUTSIDE the output (an empty
/// store dir that reads as success) and the consumer's symlink source
/// `store-path/../bin/...` names a path that cannot exist. Map each
/// `..` component to a literal `.nn-up` directory. BOTH construction
/// sites - the producer's NIX_NINJA_OUTPUTS encoding and the consumer's
/// new_built_file - must call this, and attempt 9 (2026-08-23) is why
/// that sentence is a function rather than a comment: only the consumer
/// side learned .nn-up first, and the producer kept copying outside $out.
fn store_rel_path(build_path: &Path) -> PathBuf {
    build_path
        .components()
        .map(|c| {
            if c == std::path::Component::ParentDir {
                std::ffi::OsStr::new(".nn-up")
            } else {
                c.as_os_str()
            }
        })
        .collect()
}

fn new_built_file(derived_path: SingleDerivedPath, build_path: PathBuf) -> DerivedFile {
    let output_name = normalize_output(&build_path.to_string_lossy());
    let rel_path = store_rel_path(&build_path);
    DerivedFile {
        derived_path: SingleDerivedPath::Built {
            drv_path: Arc::new(derived_path),
            output: output_name.parse().expect("invalid output name"),
        },
        build_path,
        rel_path: Some(rel_path),
    }
}

// Derivation outputs cannot have `/` in them as its suffixed to the derivation
// store path. More generally, nix store-path names admit only
// [A-Za-z0-9+._?=-] and must not start with a period: anything else
// (npm scope dirs like node_modules/@babel, mac assets like icon@2x.png,
// gettext's sr@latin.po) makes the daemon's AddToStore refuse the path,
// which poisons every target that transitively snapshots the file. The
// name is only a label - identity is the content hash - so a lossy map
// is sound; two names colliding after the map still yield distinct
// store paths unless their content is also identical.
fn normalize_output(output: &str) -> String {
    let mapped: String = output
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '.' | '_' | '?' | '=' => c,
            _ => '-',
        })
        .collect();
    if mapped.is_empty() {
        "source".to_string()
    } else if mapped.starts_with('.') {
        format!("-{mapped}")
    } else {
        mapped
    }
}

/// Whether a ninja edge is a COMPILE, for the four closure-narrowing rules
/// that say "compile" and used to test `deps == gcc`. Kept a free function
/// over names so the CMake case that broke the old test is expressible in a
/// unit test without constructing a graph.
///
/// Object-shaped outputs are the discriminator, and the polarity is
/// deliberate: anything not provably a compile answers false and keeps the
/// wider input set, which is what every edge had before the gate existed.
/// A link misread as a compile fails to build; a link correctly excluded
/// only carries more inputs.
fn is_compile_task<'a>(deps: Option<&str>, mut outs: impl Iterator<Item = &'a str>) -> bool {
    if deps != Some("gcc") {
        return false;
    }
    let object_shaped = |n: &str| {
        n.ends_with(".o")
            || n.ends_with(".obj")
            || n.ends_with(".lo")
            || n.ends_with(".gch")
            || n.ends_with(".pch")
    };
    match outs.next() {
        None => false,
        Some(first) => object_shaped(first) && outs.all(object_shaped),
    }
}

#[cfg(test)]
mod compile_task_tests {
    use super::is_compile_task;

    // THE CASE THAT REGRESSED. CMake emits link depfiles, so its
    // CMakeFiles/rules.ninja carries `deps = gcc` on the LINKER rules as
    // well as the compiler ones - read off a generated file 2026-08-26,
    // rule C_SHARED_LIBRARY_LINKER__capstone_shared_Release. Classifying
    // that as a compile withheld the implicit-input blanket from every
    // link, and zlib-ng died on a configure-generated version script its
    // edge does not declare.
    #[test]
    fn a_link_with_gcc_deps_is_not_a_compile() {
        assert!(!is_compile_task(
            Some("gcc"),
            ["libz-ng.so.2.3.3"].into_iter()
        ));
        assert!(!is_compile_task(Some("gcc"), ["libcapstone.a"].into_iter()));
        assert!(!is_compile_task(Some("gcc"), ["zlib-ng-test"].into_iter()));
    }

    #[test]
    fn a_compile_still_is_one() {
        assert!(is_compile_task(
            Some("gcc"),
            ["CMakeFiles/z.dir/deflate.c.o"].into_iter()
        ));
        assert!(is_compile_task(Some("gcc"), [".libs/hf_64.lo"].into_iter()));
        assert!(is_compile_task(Some("gcc"), ["a.o", "b.obj"].into_iter()));
    }

    // A mixed edge is not a compile: one non-object output is enough,
    // because the rules this gates are all about a TU's closure.
    #[test]
    fn mixed_outputs_are_not_a_compile() {
        assert!(!is_compile_task(
            Some("gcc"),
            ["a.o", "libz.so"].into_iter()
        ));
    }

    // No outputs cannot be a compile, and the old expression would have
    // said true for it: `all` over an empty iterator is vacuously true.
    #[test]
    fn no_outputs_is_not_a_compile() {
        let empty: [&str; 0] = [];
        assert!(!is_compile_task(Some("gcc"), empty.into_iter()));
    }

    // The deps half still gates: a custom command producing an object is
    // not a compile edge, and never was.
    #[test]
    fn other_deps_are_never_a_compile() {
        assert!(!is_compile_task(None, ["x.o"].into_iter()));
        assert!(!is_compile_task(Some("msvc"), ["x.o"].into_iter()));
    }
}

#[cfg(test)]
mod job_permits_tests {
    use super::{admission_weight, budget_for_load, budget_for_memory, JobPermits};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn concurrency_never_exceeds_cap() {
        let permits = Arc::new(JobPermits::new_without_memory_gate(3));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..20 {
            let permits = permits.clone();
            let running = running.clone();
            let peak = peak.clone();
            handles.push(std::thread::spawn(move || {
                let _permit = permits.acquire_weighted(1);
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(running.load(Ordering::SeqCst), 0, "all tasks completed");
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak concurrency {} exceeded cap 3",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "peak {} suspiciously low - the test exercised no parallelism",
            peak.load(Ordering::SeqCst)
        );
    }

    /// The weight curve is the whole point of the change: one round must run
    /// the shallow strata wide and the deep ones nearly serial. A weight of 0
    /// admits unboundedly and a weight above the budget never admits at all,
    /// and both present only as a stall, so the boundaries are pinned.
    #[test]
    fn weight_tracks_input_count_and_stays_inside_the_budget() {
        assert_eq!(admission_weight(0, 24), 1, "a leaf task must be weight 1");
        assert_eq!(admission_weight(20, 24), 1, "tens of inputs is still 1");
        // The 6,134-input realise measured in round 87: heavy enough that only
        // a few run together, not so heavy that it serializes the machine.
        let deep = admission_weight(6134, 24);
        assert!(
            (8..=16).contains(&deep),
            "a 6,134-input task weighed {deep}, outside the intended band"
        );
        // Never zero, never past the budget - the two stalling failures.
        for inputs in [0, 1, 511, 512, 513, 100_000] {
            for budget in [1, 2, 24] {
                let w = admission_weight(inputs, budget);
                assert!(w >= 1, "weight 0 for {inputs}/{budget} admits unboundedly");
                assert!(w <= budget, "weight {w} exceeds budget {budget}");
            }
        }
    }

    /// The taper is the point: a step function over a smooth resource makes
    /// the machine oscillate - admit wide, overshoot, collapse to serial,
    /// recover, repeat. The two ends and monotonicity are what must hold.
    #[test]
    fn budget_tapers_with_memory_instead_of_cliffing() {
        let (cap, reserve, full) = (24usize, 6u64, 15u64);
        assert_eq!(budget_for_memory(cap, 30, reserve, full), 24, "plenty free");
        assert_eq!(
            budget_for_memory(cap, 15, reserve, full),
            24,
            "at full mark"
        );
        assert_eq!(
            budget_for_memory(cap, 6, reserve, full),
            1,
            "at the reserve"
        );
        assert_eq!(budget_for_memory(cap, 2, reserve, full), 1, "below reserve");
        // Strictly non-decreasing in available memory, and never outside
        // [1, cap] - a 0 hangs the round, a value over cap thrashes it.
        let mut prev = 0;
        for avail in 0..=40 {
            let b = budget_for_memory(cap, avail, reserve, full);
            assert!((1..=cap).contains(&b), "budget {b} outside [1,{cap}]");
            assert!(b >= prev, "budget fell from {prev} to {b} as memory ROSE");
            prev = b;
        }
        // Mid-range must actually be in the middle, not pinned to an end -
        // otherwise the taper is a cliff wearing a linear formula.
        let mid = budget_for_memory(cap, 10, reserve, full);
        assert!(
            (2..cap).contains(&mid),
            "mid-range budget {mid} is an endpoint"
        );
    }

    /// `-l` is ninja's contract: stop starting work above the limit. 0.0
    /// disables it, which is the default and must never throttle.
    #[test]
    fn load_limit_matches_ninja_and_zero_disables() {
        assert_eq!(budget_for_load(24, 99.0, 0.0), 24, "0.0 must disable -l");
        assert_eq!(
            budget_for_load(24, 3.0, 8.0),
            24,
            "under the limit runs wide"
        );
        assert_eq!(budget_for_load(24, 8.0, 8.0), 1, "at the limit throttles");
        assert_eq!(
            budget_for_load(24, 40.0, 8.0),
            1,
            "over the limit throttles"
        );
        // Never zero: a throttled round must crawl, never stop.
        for load in [0.0, 1.0, 7.9, 8.0, 100.0] {
            assert!(budget_for_load(24, load, 8.0) >= 1);
        }
    }

    /// A machine that reports nothing must not serialize the round: unknown
    /// memory means fall back to the asked-for cap, never to 1.
    #[test]
    fn degenerate_bounds_fall_back_to_the_full_cap() {
        assert_eq!(budget_for_memory(24, 0, 0, 0), 24, "unreadable meminfo");
        assert_eq!(budget_for_memory(1, 0, 6, 15), 1, "cap of 1 stays 1");
    }

    /// A heavy task must still be admitted when it alone exceeds the budget,
    /// or a low-memory moment turns into a hang instead of a slowdown.
    #[test]
    fn an_oversized_task_still_runs_alone() {
        let permits = std::sync::Arc::new(JobPermits::new_without_memory_gate(2));
        let _p = permits.acquire_weighted(usize::MAX);
        // Acquiring it at all proves the empty-pool escape hatch works.
    }

    #[test]
    fn permit_released_on_panic() {
        let permits = Arc::new(JobPermits::new_without_memory_gate(1));
        let p2 = permits.clone();
        let _ = std::thread::spawn(move || {
            let _permit = p2.acquire_weighted(1);
            panic!("task died");
        })
        .join();
        // If the panicking thread leaked its permit, this blocks forever
        // and the test times out; acquiring proves the Drop ran.
        let _permit = permits.acquire_weighted(1);
    }
}

#[cfg(test)]
mod python_import_names_tests {
    use super::python_import_names;

    #[test]
    fn finds_import_and_from_first_segment() {
        let d = std::env::temp_dir().join(format!("nn-imports-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("__init__.py"),
            "import os\nfrom markupsafe import Markup\nfrom .environment import E\nimport sys, re\n",
        )
        .unwrap();
        let names = python_import_names(&d).unwrap();
        assert!(names.contains(&"markupsafe".to_string()));
        assert!(names.contains(&"os".to_string()));
        // relative import's first segment is empty and must not appear
        assert!(!names.contains(&"".to_string()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    // The protoc-gen-ts_proto.py shape: an executable env-shebang
    // script exec'd directly inside a sandbox with no /usr/bin/env.
    // The negative half matters as much: a non-executable file and an
    // exotic shebang must both pass through unpatched.
    #[test]
    fn env_shebang_patched_only_when_exec_and_simple() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("nn-shebang-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let exec = d.join("plugin.py");
        std::fs::write(&exec, "#!/usr/bin/env sh\nbody\n").unwrap();
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        let patched = super::patched_env_shebang(&exec).unwrap().expect("patched");
        let out = std::fs::read_to_string(&patched).unwrap();
        assert!(out.starts_with("#!/") && !out.contains("/usr/bin/env"));
        assert!(out.ends_with("body\n"));
        assert_ne!(
            std::fs::metadata(&patched).unwrap().permissions().mode() & 0o111,
            0
        );
        let plain = d.join("data.py");
        std::fs::write(&plain, "#!/usr/bin/env sh\nbody\n").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(super::patched_env_shebang(&plain).unwrap().is_none());
        let exotic = d.join("exotic.py");
        std::fs::write(&exotic, "#!/usr/bin/env -S sh -e\nbody\n").unwrap();
        std::fs::set_permissions(&exotic, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::patched_env_shebang(&exotic).unwrap().is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    // The json_parse.py shape: a MANDATORY import indented inside a
    // try/finally whose only job is restoring sys.path around it. The
    // column-0-only scanner missed it (65 task failures, round 66).
    #[test]
    fn finds_indented_import_in_try_block() {
        let d = std::env::temp_dir().join(format!("nn-imports-ind-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("json_parse.py"),
            "import sys\ntry:\n  sys.path.insert(0, p)\n  import json_comment_eater\nfinally:\n  sys.path = s\n",
        )
        .unwrap();
        let names = python_import_names(&d).unwrap();
        assert!(names.contains(&"json_comment_eater".to_string()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    // The syncqt shape (round 74): a command that cd's into a subdir and
    // reads an @-file whose content carries absolute host paths. The
    // rewrite must compensate every emitted ../ chain by the cd depth,
    // and the in-build-dir prefix (ups=0 uncompensated) must gain
    // exactly the compensation - that arithmetic is the fix.
    #[test]
    fn ancestor_rewrite_compensates_for_cd_depth() {
        use super::{rewrite_ancestor_paths, rewrite_ancestor_paths_ups};
        use std::path::Path;
        let bd = Path::new("/work/qt/build");
        let raw = "/work/qt/build/src/core/api/x\n/work/qt/src/core/api\n";
        assert_eq!(
            rewrite_ancestor_paths_ups(raw, bd, 3),
            "../../../src/core/api/x\n../../../../src/core/api\n"
        );
        // ups=0 delegation unchanged: in-build paths lose the prefix
        // entirely, the source sibling climbs one.
        assert_eq!(
            rewrite_ancestor_paths(raw, bd),
            "src/core/api/x\n../src/core/api\n"
        );
        // The whole-command form: cd target root-relative, everything
        // after the && compensated by its depth (the syncqt-then-touch
        // chain that cost round 74 two re-launches).
        use super::rewrite_cmdline;
        assert_eq!(
            rewrite_cmdline(
                "cd /work/qt/build/src/core/api && tool @/work/qt/build/src/core/api/a && touch /work/qt/build/src/core/api/ts",
                bd
            ),
            "cd src/core/api && tool @../../../src/core/api/a && touch ../../../src/core/api/ts"
        );
        // THE syncqt INCLUDE ROOT, taken from the outputs the rule already
        // declares. Two agreeing outputs name it; one does not, because a
        // single output would make its own parent the "common" root.
        use super::common_include_root;
        use std::path::PathBuf as PB;
        assert_eq!(
            common_include_root(&[
                PB::from("include/QtSvg/QtSvgDepends"),
                PB::from("include/QtSvg/qtsvgversion.h"),
                PB::from("src/svg/Svg_syncqt_timestamp"),
            ]),
            Some(PB::from("include/QtSvg"))
        );
        // NEGATIVE CONTROLS. A rule with no include outputs declares no tree;
        // two different module trees are not one root and guessing between
        // them would declare a directory the rule does not own.
        assert_eq!(
            common_include_root(&[PB::from("src/svg/Svg_syncqt_timestamp")]),
            None
        );
        assert_eq!(
            common_include_root(&[PB::from("include/QtSvg/a.h"), PB::from("include/QtGui/b.h"),]),
            None
        );
        // ONE include output IS enough - syncqt's own edge declares exactly
        // one, and requiring two rejected the only rule that matters.
        assert_eq!(
            common_include_root(&[
                PB::from("include/QtSvg/only.h"),
                PB::from("src/svg/Svg_syncqt_timestamp"),
            ]),
            Some(PB::from("include/QtSvg"))
        );
        // A REPEATED OUTPUT IS DROPPED, ORDER PRESERVED. The positional
        // encoding on the other side makes order load-bearing, so a
        // dedupe that reorders would be a different defect.
        use super::dedup_paths;
        use std::path::PathBuf;
        assert_eq!(
            dedup_paths(&[
                PathBuf::from("src/svg/Svg.version"),
                PathBuf::from("include/QtSvg/QtSvg"),
                PathBuf::from("src/svg/Svg.version"),
            ]),
            vec![
                PathBuf::from("src/svg/Svg.version"),
                PathBuf::from("include/QtSvg/QtSvg"),
            ]
        );
        // NEGATIVE CONTROL: a list with no repeat must come back untouched,
        // or the helper is deleting outputs rather than duplicates.
        let distinct = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        assert_eq!(dedup_paths(&distinct), distinct);
        // No cd prologue: plain rewrite.
        assert_eq!(rewrite_cmdline("tool /work/qt/build/x", bd), "tool x");
        // A CD TARGET ABOVE THE BUILD DIR - qtsvg's version-script rule.
        // The tail names a file GENERATED into the build dir, so from the
        // source subdir it has to climb out and back down. Before this
        // case existed the command fell through to the plain rewrite and
        // the tail came out build-dir-relative under a different cwd.
        // A CD TARGET ABOVE THE BUILD DIR: the prologue is DROPPED and the
        // tail is left exactly as the plain rewrite spells it, so the
        // command runs in the build dir where those paths resolve.
        assert_eq!(
            rewrite_cmdline(
                "cd /work/qt/src/svg && cmake -DIN_FILE=/work/qt/build/src/svg/Svg.version.in -P /nix/store/x/G.cmake",
                bd
            ),
            "cmake -DIN_FILE=src/svg/Svg.version.in -P /nix/store/x/G.cmake"
        );
        // NEGATIVE CONTROL: a store path must survive untouched, because the
        // rewrite stops above three components and a task's tools are all
        // under /nix/store.
        assert_eq!(
            rewrite_cmdline("cd /work/qt/src/svg && /nix/store/t/bin/tool", bd),
            "/nix/store/t/bin/tool"
        );
        // AND THE DESCENT CASE IS UNTOUCHED, which is what bounds this
        // change: its prologue stays and its tail keeps the depth
        // compensation every downstream pass reads.
        assert!(rewrite_cmdline(
            "cd /work/qt/build/src/core/api && tool @/work/qt/build/src/core/api/a",
            bd
        )
        .starts_with("cd src/core/api && "));
    }

    // The idl_parser shape: sys.path.insert into the importer's OWN
    // subtree, path carried through a variable (json_schema_compiler ->
    // ppapi/generators/idl_parser.py, 64 task failures, round 72). The
    // probe must find a bare module two levels down, and a negative
    // control must stay empty.
    #[test]
    fn finds_module_in_own_subtree() {
        let d = std::env::temp_dir().join(format!("nn-below-{}", std::process::id()));
        std::fs::create_dir_all(d.join("ppapi/generators")).unwrap();
        std::fs::write(d.join("ppapi/generators/idl_parser.py"), "x = 1\n").unwrap();
        let hit = super::find_module_below(&d, "idl_parser", 3).unwrap();
        assert_eq!(hit, d.join("ppapi/generators"));
        assert!(super::find_module_below(&d, "no_such_module", 3).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod rewrite_ancestor_paths_tests {
    use super::rewrite_ancestor_paths;
    use std::path::Path;

    // The gen_icui18n_shim shape: build dir 5 deep under the work root,
    // an argument pointing at the work root's src tree.
    #[test]
    fn deep_ancestor_becomes_up_chain() {
        let bd = Path::new("/work/qtwe/build/src/core/Release/x86_64");
        let cmd = "python3 gen.py --headers-root /work/qtwe/src/3p/icu/unicode --out gen";
        assert_eq!(
            rewrite_ancestor_paths(cmd, bd),
            "python3 gen.py --headers-root ../../../../../src/3p/icu/unicode --out gen"
        );
    }

    #[test]
    fn build_dir_itself_strips_to_relative() {
        let bd = Path::new("/work/qtwe/build/out");
        assert_eq!(
            rewrite_ancestor_paths("cp /work/qtwe/build/out/a.h b.h", bd),
            "cp a.h b.h"
        );
    }

    #[test]
    fn store_and_system_paths_untouched() {
        let bd = Path::new("/work/qtwe/build/out");
        let cmd = "/nix/store/abc-python/bin/python3 /bin/sh /home/x";
        assert_eq!(rewrite_ancestor_paths(cmd, bd), cmd);
    }
}

#[cfg(test)]
mod depfile_read_back_tests {
    use super::depfile_read_back;
    use std::path::PathBuf;

    #[test]
    fn fresh_stale_and_empty() {
        let d = std::env::temp_dir().join(format!("nndf{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("a.c");
        std::fs::write(&src, "int x;\n").unwrap();
        let dep = d.join("a.o.d");
        std::fs::write(&dep, "a.o: a.c \\\n hdr/one.h\n").unwrap();
        // Fresh depfile (written after the source): its answer is used.
        let got = depfile_read_back(
            &d,
            Some(PathBuf::from("a.o.d").as_path()),
            &[PathBuf::from("a.c")],
        )
        .unwrap();
        assert_eq!(got, vec![PathBuf::from("a.c"), PathBuf::from("hdr/one.h")]);
        // Source newer than the depfile: no answer, fall back to the scan.
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::open(&src)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(newer))
            .unwrap();
        assert!(depfile_read_back(
            &d,
            Some(PathBuf::from("a.o.d").as_path()),
            &[PathBuf::from("a.c")]
        )
        .is_none());
        // Empty depfile is no answer, not an empty answer.
        std::fs::write(&dep, "").unwrap();
        assert!(depfile_read_back(&d, Some(PathBuf::from("a.o.d").as_path()), &[]).is_none());
        // No depfile declared: the scan is the only source.
        assert!(depfile_read_back(&d, None, &[]).is_none());
    }
}

#[cfg(test)]
mod new_built_file_tests {
    use super::new_built_file;
    use harmonia_store_derivation::derived_path::SingleDerivedPath;
    use harmonia_store_path::StorePath;
    use std::path::PathBuf;

    fn dummy_drv() -> SingleDerivedPath {
        SingleDerivedPath::Opaque(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-x.drv"
                .parse::<StorePath>()
                .unwrap(),
        )
    }

    #[test]
    fn wl_groups_yield_files_and_skip_output_flags() {
        // json-c, 2026-08-23: the version script travels inside a -Wl
        // group. Both spellings parse; output-taking flags' values are
        // never candidates in either spelling; -soname's value IS a
        // candidate (the caller's existence check rejects it).
        assert_eq!(
            super::wl_file_candidates("--version-script,/src/json-c.sym"),
            vec!["/src/json-c.sym"]
        );
        assert_eq!(
            super::wl_file_candidates("--version-script=/src/json-c.sym"),
            vec!["/src/json-c.sym"]
        );
        assert_eq!(
            super::wl_file_candidates("--dependency-file,x/link.d,-soname,libx.so.5"),
            vec!["libx.so.5"]
        );
        assert_eq!(
            super::wl_file_candidates("--dependency-file=x/link.d,-Bsymbolic-functions"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn rel_path_never_climbs() {
        // An out-of-build-dir output keeps its climb in build_path (a
        // sandbox location) and must NOT keep it in rel_path (a location
        // inside the store output): carried verbatim, the producer copies
        // to $out/../... (outside the output, silently empty result) and
        // the consumer reads store-path/../... (cannot exist). openfec,
        // server edition attempt 8, 2026-08-23.
        let df = new_built_file(
            dummy_drv(),
            PathBuf::from("../bin/Release/libopenfec.so.1.4.2"),
        );
        assert_eq!(
            df.build_path,
            PathBuf::from("../bin/Release/libopenfec.so.1.4.2")
        );
        assert_eq!(
            df.rel_path,
            Some(PathBuf::from(".nn-up/bin/Release/libopenfec.so.1.4.2"))
        );
    }

    #[test]
    fn in_tree_output_keeps_rel_path_equal() {
        let df = new_built_file(dummy_drv(), PathBuf::from("src/main.o"));
        assert_eq!(df.rel_path, Some(PathBuf::from("src/main.o")));
    }
}

#[cfg(test)]
mod normalize_output_tests {
    use super::normalize_output;

    #[test]
    fn slash_still_maps_to_dash() {
        assert_eq!(normalize_output("src/main.o"), "src-main.o");
    }

    #[test]
    fn at_sign_is_sanitized() {
        // The three shapes measured in the wild: mac asset suffixes,
        // npm scope directories, gettext locale variants.
        assert_eq!(normalize_output("icon@2x.png"), "icon-2x.png");
        assert_eq!(
            normalize_output("node_modules/@babel/core/lib/index.js"),
            "node_modules--babel-core-lib-index.js"
        );
        assert_eq!(normalize_output("sr@latin.po"), "sr-latin.po");
    }

    #[test]
    fn allowed_charset_passes_through() {
        assert_eq!(
            normalize_output("libfoo+bar_1.2-r3?x=y.so"),
            "libfoo+bar_1.2-r3?x=y.so"
        );
    }

    #[test]
    fn leading_period_is_prefixed() {
        assert_eq!(normalize_output(".hidden.c"), "-.hidden.c");
    }

    #[test]
    fn empty_falls_back() {
        assert_eq!(normalize_output(""), "source");
    }

    #[test]
    fn spaces_and_unicode_are_sanitized() {
        assert_eq!(normalize_output("a b\u{e9}.c"), "a-b-.c");
    }

    #[test]
    fn syspath_dirs_read_quoted_segments() {
        use super::python_syspath_dirs;
        use std::fs;
        let root = std::env::temp_dir().join(format!("sp-test-{}", std::process::id()));
        let tools = root.join("tools/polymer");
        let node = root.join("third_party/node");
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir_all(&node).unwrap();
        fs::write(
            tools.join("css_to_wrapper.py"),
            "import sys, os\nsys.path.append(os.path.join(_HERE_PATH, '..', '..', 'third_party', 'node'))\nimport node\n",
        )
        .unwrap();
        let dirs = python_syspath_dirs(&tools).unwrap();
        assert_eq!(dirs, vec![node]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn generate_grd_cmdline_files_resolve_against_tool_root() {
        use super::generate_grd_input_files;
        use std::fs;
        let root = std::env::temp_dir().join("nn-grd-input-files-test");
        let imgs = root.join("ui/webui/resources/images");
        fs::create_dir_all(&imgs).unwrap();
        fs::create_dir_all(root.join("ui/webui/resources/tools")).unwrap();
        fs::write(imgs.join("add.svg"), "<svg/>").unwrap();
        let cmd = format!(
            "python3 {r}/ui/webui/resources/tools/generate_grd.py \
             --out-grd gen/x/resources.grdp --grd-prefix webui_images \
             --root-gen-dir gen --input-files-base-dir ui/webui/resources/images \
             --input-files add.svg missing.svg --resource-path-prefix images",
            r = root.display()
        );
        let got = generate_grd_input_files(&cmd);
        // missing.svg fails the existence filter; add.svg resolves.
        assert_eq!(got, vec![imgs.join("add.svg")]);
        // No --input-files at all (the .grd aggregation edges): empty.
        assert!(generate_grd_input_files("python3 x/generate_grd.py --out-grd y").is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lexical_join_pops_parents_against_base() {
        use super::lexical_join;
        use std::path::Path;
        assert_eq!(
            lexical_join(
                Path::new("gen/ui/webui/resources/js"),
                Path::new("../../../../../../../../../../src/x.d.ts"),
            ),
            Path::new("../../../../../src/x.d.ts")
        );
        assert_eq!(
            lexical_join(Path::new("gen/a"), Path::new("b/./c")),
            Path::new("gen/a/b/c")
        );
    }

    #[test]
    fn grd_scaled_image_resolves_through_context_dir() {
        use super::grd_references;
        use std::fs;
        let dir = std::env::temp_dir().join(format!("grd-test-{}", std::process::id()));
        let scaled = dir.join("default_100_percent/flags_ui");
        fs::create_dir_all(&scaled).unwrap();
        fs::write(scaled.join("favicon.png"), b"png").unwrap();
        fs::write(dir.join("direct.xtb"), b"xtb").unwrap();
        let grd = dir.join("res.grd");
        fs::write(
            &grd,
            r#"<grit><outputs><output context="default_100_percent"/></outputs>
               <file path="direct.xtb"/>
               <structure type="chrome_scaled_image" file="flags_ui/favicon.png"/>
               <structure type="chrome_scaled_image" file="flags_ui/absent.png"/></grit>"#,
        )
        .unwrap();
        let refs = grd_references(&grd).unwrap();
        assert!(refs.contains(&dir.join("direct.xtb")));
        assert!(refs.contains(&scaled.join("favicon.png")));
        assert_eq!(refs.len(), 2, "absent file must not be invented: {refs:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

/// Discovers C include dependencies from a command line and input files.
/// Returns (discovered_deps, discovered_store_paths) where:
/// - discovered_deps: DerivedFiles that need to be encoded and added to NIX_NINJA_INPUTS
/// - discovered_store_paths: Store paths that only need to be added as input sources
///
/// The freshness-guarded depfile parse behind upstream #17. Returns None -
/// meaning "run the scan" - unless the depfile exists and is at least as
/// new as every source file offered, and parses cleanly.
fn depfile_read_back(
    build_dir: &Path,
    depfile: Option<&Path>,
    sources: &[PathBuf],
) -> Option<Vec<PathBuf>> {
    let d = depfile?;
    let d = if d.is_absolute() {
        d.to_path_buf()
    } else {
        build_dir.join(d)
    };
    let dep_m = std::fs::metadata(&d).ok()?.modified().ok()?;
    for srcf in sources {
        let p = if srcf.is_absolute() {
            srcf.clone()
        } else {
            build_dir.join(srcf)
        };
        let m = std::fs::metadata(&p).ok()?.modified().ok()?;
        if m > dep_m {
            return None;
        }
    }
    let buf = n2::scanner::read_file_with_nul(&d).ok()?;
    let mut sc = n2::scanner::Scanner::new(&buf);
    let parsed = n2::depfile::parse(&mut sc).ok()?;
    let mut out: Vec<PathBuf> = Vec::new();
    for (_, values) in parsed.iter() {
        for v in values {
            out.push(PathBuf::from(v));
        }
    }
    // An empty parse is no answer, not an empty answer: fall back to the
    // scan rather than declaring a TU with zero includes.
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Union of the textual scan and the preprocessor's depfile, scan order
/// first, deduped. See the call site for why neither alone is sufficient.
fn merge_scan_and_preprocessor(scanned: Vec<PathBuf>, preprocessed: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = scanned.iter().cloned().collect();
    let mut out = scanned;
    for p in preprocessed {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

pub fn discover_c_includes(
    rpc_client: &Arc<BuilderRpcClient>,
    store_dir: &StoreDir,
    build_dir: &Path,
    cmdline: &str,
    files: Vec<PathBuf>,
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
    depfile: Option<&Path>,
) -> Result<(Vec<DerivedFile>, Vec<StorePath>)> {
    // UPSTREAM #17, THE READ-BACK HALF: a depfile already on disk is the
    // COMPILER'S OWN answer to the question the BFS scan approximates, so
    // when one exists and is FRESH it replaces the scan outright. Fresh
    // means at least as new as every source it would speak for: a stale
    // depfile under-declares the include a later edit added, and the guard
    // fails toward the scan (the old behavior), never toward trusting a
    // stale answer. Incremental local builds are where this pays - ninja's
    // own model, a depfile per object from the previous run.
    let c_includes = match depfile_read_back(build_dir, depfile, &files) {
        Some(deps) => deps,
        None => {
            let (scanned, incomplete) = c_include_parser::retrieve_c_includes_checked(
                cmdline,
                files.clone(),
                virtual_paths,
            )?;
            // THE SCAN NOW SAYS WHEN IT CANNOT ANSWER, AND THIS IS WHAT
            // ACTS ON IT. A computed include through a function-like macro
            // needs real expansion against the command line's -D set, which
            // a textual parser cannot do and which the preprocessor does by
            // definition. Measured 2026-08-24 on cmake-minimal's bootstrap:
            // `#include KWSYS_HEADER(Directory.hxx)` with
            // -DKWSYS_NAMESPACE=cmsys, so every kwsys TU reached its task
            // with no cmsys/ header declared and died "No such file or
            // directory" - the scan's silence read as a complete answer.
            //
            // It runs for those TUs ALONE. The scan is exact for the
            // overwhelming majority, and paying a preprocess per object
            // would hand back the time per-TU derivations exist to save.
            //
            // The fallback's polarity is deliberate: if gcc itself fails
            // here, keep the scan's answer and let the TASK fail loudly.
            // Substituting an empty or partial set on a failed preprocess
            // would turn a build error into a wrong artifact, which is the
            // one outcome worse than the bug being fixed.
            if incomplete {
                match deps_infer::gcc_depfile::retrieve_c_includes(cmdline) {
                    Ok(deps) => {
                        // THE PREPROCESSOR ADDS TO THE SCAN, IT DOES NOT REPLACE IT.
                        // `-MM` (include_system_headers: false) omits every header gcc
                        // considers a SYSTEM header, and that is a property of the
                        // FILE rather than of where it lives: gnulib writes
                        // `#pragma GCC system_header` into the replacement headers it
                        // GENERATES INTO THE BUILD DIRECTORY. So the depfile answer
                        // silently drops build-dir files that must be materialized.
                        // Measured 2026-08-24 on gnum4-1.4.21: `gcc -MM -I. quotearg.c`
                        // declares 15 headers with lib/wchar.h and lib/limits.h absent,
                        // `gcc -M` lists both, and the textual scan finds both. The
                        // task then compiled against the SYSTEM wchar.h and died
                        // `implicit declaration of function 'mbszero'` - a wrong-header
                        // failure wearing a missing-prototype message, because an angle
                        // include that was never materialized SUCCEEDS at finding the
                        // wrong file.
                        //
                        // Neither answer is a superset of the other: the scan is blind
                        // to computed includes (cmake's kwsys), the depfile is blind to
                        // pragma-marked build-dir headers (every autotools package
                        // carrying gnulib). Union them, scan order first. Over-declaring
                        // is the pipeline's safe polarity - an extra input costs one
                        // upload - while under-declaring is silent and ships the wrong
                        // artifact.
                        let merged = merge_scan_and_preprocessor(scanned, deps);
                        eprintln!(
                            "nix-ninja: computed include unresolvable by scan; \
                             scan and preprocessor union to {} input(s)",
                            merged.len()
                        );
                        merged
                    }
                    Err(e) => {
                        eprintln!(
                            "nix-ninja: computed include unresolvable by scan and the \
                             preprocessor fallback failed ({e}); keeping the scan's {} \
                             input(s), so the task fails loudly rather than silently \
                             building with the wrong ones",
                            scanned.len()
                        );
                        scanned
                    }
                }
            } else {
                scanned
            }
        }
    };
    let mut discovered_deps = Vec::new();
    let mut discovered_store_paths = Vec::new();
    let mut to_upload: Vec<PathBuf> = Vec::new();

    // Convert input files to a set for filtering
    let input_files: HashSet<PathBuf> = files.into_iter().collect();

    // Declared-and-opened, deduped: a header reached twice in one preprocess
    // is one input kept, and counting occurrences could push the numerator
    // past its denominator.
    let mut used_declared: HashSet<PathBuf> = HashSet::new();

    for include in c_includes {
        // Skip input files - we only want to discover new dependencies
        if input_files.contains(&include) {
            // THIS BRANCH IS THE MEASUREMENT: reaching it means the include
            // was declared and the preprocessor opened it. See DECLARED_HEADERS.
            if header_like(&include) {
                used_declared.insert(include.clone());
            }
            continue;
        }

        // Check if include is from Nix store or a regular file
        if let Ok(relative) = include.strip_prefix(AsRef::<Path>::as_ref(store_dir)) {
            if let Some(hash_path) = relative.components().next().map(|c| c.as_os_str()) {
                let full_path = AsRef::<Path>::as_ref(store_dir).join(hash_path);
                let store_path: StorePath =
                    store_dir.parse(full_path.to_str().context("Path was not valid UTF-8")?)?;
                discovered_store_paths.push(store_path);
                continue;
            }
        }

        // Regular file: queued for a batched store add below.
        to_upload.push(include);
    }
    discovered_deps.extend(new_opaque_files(rpc_client, build_dir, to_upload)?);

    USED_HEADERS.fetch_add(
        used_declared.len() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );

    Ok((discovered_deps, discovered_store_paths))
}

/// Removes -frandom-seed flag from a string of CFLAGS.
/// Drop every `-rpath <path>` pair whose path names one of the OUTER
/// derivation's outputs ($out, $dev, ... as the `outputs` env lists them).
/// Other rpath entries (store libraries) are inputs and stay.
fn remove_outer_rpath(value: &str) -> String {
    let outer = outer_output_paths();
    let toks: Vec<&str> = value.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == "-rpath"
            && i + 1 < toks.len()
            && outer.iter().any(|o| toks[i + 1].starts_with(o.as_str()))
        {
            i += 2;
            continue;
        }
        out.push(toks[i]);
        i += 1;
    }
    out.join(" ")
}

/// The build-path field of an encoded `store:build_path:rel` input.
fn encoded_build_path(e: &str) -> &str {
    e.split(':').nth(1).unwrap_or(e)
}

fn remove_frandom_seed(flags: &str) -> String {
    flags
        .split_whitespace()
        .filter(|flag| !flag.starts_with("-frandom-seed="))
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Generates -frandom-seed based on the task's cmdline.
fn generate_frandom_seed(cmdline: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cmdline.as_bytes());
    let result = hasher.finalize();
    format!("{result:x}")[..16].to_string()
}

/// The body of [`Runner::resolve_target`], taking its two maps directly so it
/// can be tested without standing up a Runner (which needs a store, an RPC
/// client and a populated graph).
/// Whether a dependency name is a runtime-library handle worth pulling
/// into an executing task's closure: a RELATIVE `.so`/`.so.N` path (a
/// store-path lib is already visible in every sandbox) or meson's SHSYM
/// `.symbols` relink guard, the only edge-visible handle on the lib it
/// certifies.
/// An absolute path candidate in a cmdline argument: the arg itself, or
/// the value of a `VAR=/abs` token (CMake `-D INPUT_FILE=/...`). CMake
/// CUSTOM_COMMAND edges reference source files by absolute path and may
/// declare no inputs at all (svt-av1's version-header step, 2026-08-23),
/// so absolute spellings need their own discovery route.
fn absolute_file_candidate(arg: &str) -> Option<&str> {
    if arg.starts_with('/') {
        return Some(arg);
    }
    arg.split_once('=')
        .map(|(_, v)| v)
        .filter(|v| v.starts_with('/'))
}

/// Whether `target` sits in the same top-level tree as `base` (their
/// first real path component agrees) - a target sharing only the root
/// (`/etc/...` against `/build/...`) is outside the project tree and not
/// ours to upload.
fn same_project_tree(base: &Path, target: &Path) -> bool {
    let first = |p: &Path| {
        p.components().find_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_owned()),
            _ => None,
        })
    };
    match (first(base), first(target)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Whether a compile command will actually WRITE a depfile. CMake's LTO
/// capability probe (`_CMakeLTOTest-CXX`) generates an edge declaring
/// `deps = gcc` and a depfile while its command carries no -MD/-MF, so
/// gcc never writes the file and a task that declared it as an output
/// dies collecting it (`canonicalize(...o.d): No such file`). The edge
/// declaration is a promise about the RULE; the command is the truth.
/// -MMD implies -MD's writing behaviour; -MF names the file explicitly
/// (spaced or fused). The rspfile is part of the command line.
fn command_writes_depfile(cmdline: Option<&str>, rsp: Option<&str>) -> bool {
    let writes = |s: &str| {
        s.split_whitespace().any(|t| {
            t == "-MD"
                || t == "-MMD"
                || t.starts_with("-MF")
                // The LINKER spelling: CMake 3.27+ link edges write their
                // depfile via `-Wl,--dependency-file=...` (capstone's LTO
                // link died writing link.d into a directory only the
                // declared-output path used to create).
                || t.contains("--dependency-file")
        })
    };
    cmdline.is_some_and(writes) || rsp.is_some_and(writes)
}

fn lib_shaped(name: &str) -> bool {
    if name.starts_with('/') {
        return false;
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    base.ends_with(".symbols") || base.ends_with(".so") || base.contains(".so.")
}

fn resolve_target_in(
    derived_files: &HashMap<FileId, DerivedFile>,
    phony_aliases: &HashMap<FileId, Vec<FileId>>,
    fid: FileId,
) -> Vec<DerivedFile> {
    let mut out = Vec::new();
    let mut seen: rustc_hash::FxHashSet<FileId> = rustc_hash::FxHashSet::default();
    let mut worklist = vec![fid];
    while let Some(next) = worklist.pop() {
        if !seen.insert(next) {
            continue;
        }
        // A concrete output ends the walk. Checked FIRST because a fid can be
        // both an alias target and a real output, and the real output is the
        // thing a caller asked for.
        if let Some(df) = derived_files.get(&next) {
            out.push(df.clone());
            continue;
        }
        if let Some(alias_ins) = phony_aliases.get(&next) {
            worklist.extend(alias_ins.iter().copied());
        }
    }
    out
}

#[cfg(test)]
mod target_resolution_tests {
    use super::*;

    /// The runtime-lib shape table: relative .so and .so.N and meson
    /// .symbols pull in; store-path libs, objects, and sources stay out.
    /// The orc failure row is `liborc-0.4.so.0.42.0` reached through
    /// `liborc-0.4.so.0.42.0.symbols`.
    #[test]
    fn absolute_cmdline_args_rebase_into_the_build_tree() {
        use super::absolute_file_candidate as cand;
        use crate::relative_from::relative_from;
        use std::path::{Path, PathBuf};
        // svt-av1's two spellings.
        assert_eq!(
            cand("/build/source/Source/Lib/Codec/ConfigureGitVersion.cmake"),
            Some("/build/source/Source/Lib/Codec/ConfigureGitVersion.cmake")
        );
        assert_eq!(
            cand("INPUT_FILE=/build/source/Source/Lib/Codec/EbVersion.h.in"),
            Some("/build/source/Source/Lib/Codec/EbVersion.h.in")
        );
        // Not candidates: relative, flag-shaped, VAR=relative.
        assert_eq!(cand("../Source/x.c"), None);
        assert_eq!(cand("OUTPUT_FILE=EbVersion.h"), None);
        let base = Path::new("/build/source/build");
        assert_eq!(
            relative_from(Path::new("/build/source/Source/a.cmake"), base),
            Some(PathBuf::from("../Source/a.cmake"))
        );
        // Sharing only the root is outside the project tree.
        assert!(!super::same_project_tree(base, Path::new("/etc/passwd")));
        assert!(super::same_project_tree(base, Path::new("/build/source/x")));
    }

    #[test]
    fn a_generated_header_under_include_is_not_a_tree() {
        // libvmaf's meson vcs_tag output: a FILE, not a module tree - the
        // old predicate classified it as one, keyed it under /.nn-tree,
        // and every consumer missed it in derived_files (ninth class).
        assert!(!is_tree_path(Path::new("include/vcs_version.h")));
        // The shapes the predicate exists for still match.
        assert!(is_tree_path(Path::new("include/QtSvg")));
        assert!(is_tree_path(Path::new("include/QtSvg/.nn-tree")));
        assert!(is_tree_path(Path::new("foo/bar_autogen")));
    }

    #[test]
    fn depfile_only_declared_when_the_command_writes_it() {
        use super::command_writes_depfile as w;
        // CMake LTO probe: depfile declared, command writes none.
        assert!(!w(
            Some("g++ -g -flto=auto -fPIC -o foo.o -c foo.cpp"),
            None
        ));
        // Ordinary CMake/meson compile lines.
        assert!(w(Some("gcc -MD -MT a.o -MF a.o.d -o a.o -c a.c"), None));
        assert!(w(Some("gcc -MMD -o a.o -c a.c"), None));
        assert!(w(Some("gcc -MFa.o.d -o a.o -c a.c"), None));
        // Flags riding an rspfile count as the command.
        assert!(w(Some("gcc @rsp"), Some("-MD -MF a.o.d -c a.c")));
        // The linker spelling: CMake 3.27+ link edges.
        assert!(w(
            Some("gcc -shared -Wl,--dependency-file=CMakeFiles/x.dir/link.d -o libx.so a.o"),
            None
        ));
        // A path merely CONTAINING the letters is not a flag.
        assert!(!w(Some("gcc -I/opt/x-MD/include -o a.o -c a.c"), None));
        assert!(!w(None, None));
    }

    #[test]
    fn lib_shape_table() {
        assert!(lib_shaped("orc/liborc-0.4.so.0.42.0"));
        assert!(lib_shaped("orc/liborc-0.4.so"));
        assert!(lib_shaped(
            "orc/liborc-0.4.so.0.42.0.p/liborc-0.4.so.0.42.0.symbols"
        ));
        assert!(!lib_shaped(
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-glibc/lib/libm.so"
        ));
        assert!(!lib_shaped("orc/liborc-0.4.so.0.42.0.p/orc.c.o"));
        assert!(!lib_shaped("../orc/orc.c"));
        assert!(!lib_shaped("tools/orcc"));
    }
    use harmonia_store_path::StorePath;
    use nix_ninja_task::derived_file::DerivedFile;

    fn df(name: &str) -> DerivedFile {
        DerivedFile {
            derived_path: SingleDerivedPath::Opaque(
                // from_bytes takes the BASENAME, not an absolute path. The
                // hash is 32 characters of nix-base32, whose alphabet omits
                // e/o/u/t but includes 0, so all-zeroes is a valid fixture.
                StorePath::from_bytes(
                    format!("00000000000000000000000000000000-{name}").as_bytes(),
                )
                .expect("fixture store path"),
            ),
            build_path: PathBuf::from(name),
            rel_path: None,
        }
    }

    #[test]
    fn concrete_target_resolves_to_itself() {
        let mut outs = HashMap::new();
        outs.insert(FileId::from(1), df("a.o"));
        let got = resolve_target_in(&outs, &HashMap::new(), FileId::from(1));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].build_path, PathBuf::from("a.o"));
    }

    // The gap this whole change closes. A phony records no derived file of its
    // own in this fork - it is expanded at input assembly - so before
    // resolve_target existed, naming one as a CLI target reported a missing
    // derived file for a build that had in fact succeeded.
    #[test]
    fn phony_target_resolves_to_the_files_it_aliases() {
        let mut outs = HashMap::new();
        outs.insert(FileId::from(1), df("a.o"));
        outs.insert(FileId::from(2), df("b.o"));
        let mut phony = HashMap::new();
        phony.insert(FileId::from(9), vec![FileId::from(1), FileId::from(2)]);

        let mut got = resolve_target_in(&outs, &phony, FileId::from(9));
        got.sort();
        assert_eq!(
            got.iter().map(|d| d.build_path.clone()).collect::<Vec<_>>(),
            vec![PathBuf::from("a.o"), PathBuf::from("b.o")]
        );
    }

    #[test]
    fn phony_of_phony_resolves_transitively() {
        let mut outs = HashMap::new();
        outs.insert(FileId::from(1), df("a.o"));
        let mut phony = HashMap::new();
        phony.insert(FileId::from(8), vec![FileId::from(1)]);
        phony.insert(FileId::from(9), vec![FileId::from(8)]);

        let got = resolve_target_in(&outs, &phony, FileId::from(9));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].build_path, PathBuf::from("a.o"));
    }

    // ninja lets a build file declare an alias cycle, so this is reachable
    // input rather than a defensive flourish. Without the seen-set it hangs,
    // and a hang is the one failure mode that writes no error to read.
    #[test]
    fn alias_cycle_terminates() {
        let mut phony = HashMap::new();
        phony.insert(FileId::from(1), vec![FileId::from(2)]);
        phony.insert(FileId::from(2), vec![FileId::from(1)]);
        let got = resolve_target_in(&HashMap::new(), &phony, FileId::from(1));
        assert!(got.is_empty());
    }

    // An unknown target must resolve EMPTY rather than to something
    // plausible: build() turns the empty vec into the "missing derived file"
    // error that names the target, and a silent empty success there would
    // report a build that never happened.
    #[test]
    fn unknown_target_resolves_empty() {
        let got = resolve_target_in(&HashMap::new(), &HashMap::new(), FileId::from(7));
        assert!(got.is_empty());
    }
}

#[cfg(test)]
mod prune_gate_tests {
    use super::{header_like, prune_line};
    use std::path::Path;

    #[test]
    fn header_shape_covers_the_c_family_and_nothing_else() {
        for h in ["a.h", "b.hpp", "c.hh", "d.inc", "e.ipp"] {
            assert!(header_like(Path::new(h)), "{h} should be header-shaped");
        }
        for n in ["obj.o", "gen.cc", "main.cpp", "noext"] {
            assert!(
                !header_like(Path::new(n)),
                "{n} is not this ratio's business"
            );
        }
    }

    /// The arithmetic, and the refusal. A zero numerator is the disjointness
    /// signature the first version of this shipped, so the tick must NOT
    /// render it as a ratio - "0 used / N declared" reads as N dead headers.
    #[test]
    fn a_zero_numerator_suppresses_the_ratio_instead_of_reporting_it() {
        assert_eq!(prune_line(0, 0), "", "nothing measured yet prints nothing");
        assert!(
            prune_line(900, 0).contains("SUPPRESSED"),
            "0 used against a nonzero denominator must not print a ratio"
        );
        assert_eq!(
            prune_line(900, 300),
            ", hdrs 300 used / 900 declared (600 prunable)"
        );
        // kept can never exceed declared, but the arithmetic must not panic
        // if a future edit breaks that invariant.
        assert!(prune_line(10, 40).contains("(0 prunable)"));
    }

    /// THE STRUCTURAL GATE, and the reason it reads source rather than
    /// calling anything: the defect this replaces had a passing unit test.
    /// That test reimplemented the predicate over data it supplied itself,
    /// including an overlap between the used and declared sets that the real
    /// system guarantees cannot happen. No assertion over supplied data can
    /// catch a numerator drawn from the wrong population - only the SITE can.
    ///
    /// So: the used counter must be fed from inside the taken branch of the
    /// `input_files.contains` filter, where membership in both sets IS the
    /// branch condition, and must never be fed from `discovered_deps`, which
    /// that filter defines as the complement of the declared set.
    #[test]
    fn the_numerator_is_counted_inside_the_filters_taken_branch() {
        let src = include_str!("task.rs");
        // Built at runtime so this test cannot match its own text.
        let filter = concat!("input_files.", "contains(&include)");
        let counter = concat!("used_", "declared.insert");
        let feed = concat!("USED_", "HEADERS.fetch_add");

        let lines: Vec<&str> = src.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains(filter))
            .expect("the discovery filter moved - re-read discover_c_includes");
        let branch = &lines[at..(at + 8).min(lines.len())];
        let cont = branch
            .iter()
            .position(|l| l.trim() == "continue;")
            .expect("the filter's taken branch no longer continues");
        assert!(
            branch[..cont].iter().any(|l| l.contains(counter)),
            "the used counter is not inside the filter's taken branch: it is \
             counting a population the declared set cannot intersect"
        );

        // And the complement must never feed it. Checked on the call's own
        // ARGUMENT rather than its neighbourhood: the first version of this
        // scanned six lines either side and failed on the correct code,
        // because the function returns `discovered_deps` three lines below.
        let feeds: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(feed))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            feeds.len(),
            1,
            "expected exactly one site feeding the used counter"
        );
        let args = lines[feeds[0] + 1..(feeds[0] + 4).min(lines.len())].join(" ");
        assert!(
            args.contains(concat!("used_", "declared.len()")),
            "the used counter is not fed from the deduped declared-and-opened set"
        );
        assert!(
            !args.contains("discovered_deps"),
            "the used counter is fed from discovered_deps, the declared set's complement"
        );
    }

    /// The negative control. Without it the test above passes on any file
    /// containing the two strings in any order, which is how the matcher it
    /// replaces was wrong.
    #[test]
    fn the_gate_rejects_a_count_placed_after_the_continue() {
        let bad = [
            "if input_files.contains(&include) {",
            "continue;",
            "used_declared.insert(x);",
        ];
        let at = bad
            .iter()
            .position(|l| l.contains("contains(&include)"))
            .unwrap();
        let branch = &bad[at..];
        let cont = branch.iter().position(|l| l.trim() == "continue;").unwrap();
        assert!(
            !branch[..cont]
                .iter()
                .any(|l| l.contains("used_declared.insert")),
            "the gate would accept a counter that can never run"
        );
    }
}

#[cfg(test)]
mod self_rss_tests {
    use super::self_rss_mib;

    /// A running process has a nonzero resident size, so a zero here means
    /// the read or the parse failed - which is the failure this returns 0
    /// for deliberately, and the reason a test has to distinguish them.
    #[test]
    fn reports_a_plausible_resident_size() {
        let mib = self_rss_mib();
        assert!(mib > 0, "self_rss_mib returned 0 for a live process");
        // A test binary under 8 GiB: loose enough never to flake, tight
        // enough to catch a unit error (pages read as bytes would be ~4000x).
        assert!(mib < 8192, "implausible rss {mib} MiB - check the unit");
    }

    /// The point of the pair is that it MOVES with the live set while RSS
    /// does not have to, so the check allocates and watches it. Asserting
    /// only that the call returns something would pass against a stub
    /// returning constants, which is the reading this tick exists to replace.
    ///
    /// This assertion has already earned its place: against the first version
    /// of `self_heap_mib`, which reported `uordblks` alone, it failed with
    /// `0 -> 0 MiB` because a 128 MiB allocation is served by mmap and never
    /// touches the arena. That is the whole class of defect here - a field
    /// that is plausible, documented, and answers a different question.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn in_use_tracks_a_real_allocation() {
        use super::self_heap_mib;

        let (before, _) = self_heap_mib().expect("glibc reports mallinfo2");
        // 128 MiB, well past any allocation the test harness does on its own
        // threads while this runs, so the comparison does not need a lock.
        let big: Vec<u8> = vec![7u8; 128 * 1024 * 1024];
        let (during, retained) = self_heap_mib().expect("glibc reports mallinfo2");
        assert!(
            during >= before + 64,
            "in-use heap did not move across a 128 MiB allocation: \
             {before} -> {during} MiB"
        );
        // In-use blocks are drawn from the arena or from mmap, so this
        // ordering holds by construction; it fails if the two fields are
        // ever swapped, which is the one error the numbers alone would hide.
        assert!(
            during <= retained,
            "in-use {during} MiB exceeds retained {retained} MiB - fields swapped?"
        );
        // Keep the allocation live past the second reading.
        assert_eq!(std::hint::black_box(&big).len(), 128 * 1024 * 1024);
    }
}

#[cfg(test)]
mod ninja_pool_tests {
    /// $out/$dev are PROCESS globals and cargo runs tests on parallel
    /// threads, so the two tests below - which set the same vars to
    /// DIFFERENT values - race: whichever writes last wins under the
    /// other's assertions. Latent until the sandboxed check phase hit the
    /// interleaving (remove_outer_rpath saw the rewrite-map test's $out
    /// and stripped nothing). Poisoning is survivable: a panicking holder
    /// must not fail the other test twice.
    static OUT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn cmdline_is_lto_last_flag_wins_and_fat_changes_nothing() {
        assert!(super::cmdline_is_lto("gcc -O3 -flto=8 -c a.c -o a.o"));
        assert!(super::cmdline_is_lto("gcc -flto -c a.c"));
        assert!(super::cmdline_is_lto(
            "gcc -flto=auto -ffat-lto-objects -c a.c"
        ));
        assert!(
            !super::cmdline_is_lto("gcc -flto=8 -fno-lto -c a.c"),
            "later -fno-lto wins"
        );
        assert!(
            super::cmdline_is_lto("gcc -fno-lto -flto -c a.c"),
            "later -flto wins"
        );
        assert!(!super::cmdline_is_lto("gcc -O3 -c a.c -o a.o"));
        // the placeholder-restore hazard is about IR, not the word: a
        // path containing 'flto' is not a flag
        assert!(!super::cmdline_is_lto("gcc -c /src/flto/a.c"));
    }

    #[test]
    fn task_is_lto_sees_the_wrapper_baseline_and_the_opt_out_after_it() {
        // Never read the real environment here; the baseline is
        // exercised through scan_lto_flags's start state, which is what
        // task_is_lto folds NIX_NINJA_ASSUME_LTO into.
        let mut vars: std::collections::HashMap<String, String> = Default::default();
        // no baseline, nothing on the line: not LTO
        assert!(!super::scan_lto_flags("gcc -O2 -c a.c", false));
        // wrapper baseline with a bare command line (the bison shape): LTO
        assert!(super::scan_lto_flags("gcc -O2 -c a.c", true));
        // a package's -fno-lto opt-out arrives via NIX_CFLAGS_COMPILE and
        // wins over the baseline, as the wrapper appends it last
        vars.insert("NIX_CFLAGS_COMPILE".into(), "-O3 -fno-lto".into());
        assert!(!super::scan_lto_flags(
            vars["NIX_CFLAGS_COMPILE"].as_str(),
            super::scan_lto_flags("gcc -O2 -c a.c", true)
        ));
        // and the env-driven path with no baseline set in this process
        std::env::remove_var("NIX_NINJA_ASSUME_LTO");
        assert!(!super::task_is_lto("gcc -O2 -c a.c", &vars));
        vars.insert("NIX_CFLAGS_COMPILE".into(), "-O3 -flto=8".into());
        assert!(super::task_is_lto("gcc -O2 -c a.c", &vars));
    }

    #[test]
    fn outer_rewrite_map_is_same_length_stable_and_reversible() {
        let _env = OUT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("outputs", "out dev");
        std::env::set_var(
            "out",
            "/nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-1.2.16.1",
        );
        std::env::set_var(
            "dev",
            "/nix/store/npbwm562dyvwjim6j7qa1cish2vxlqr0-alsa-lib-1.2.16.1-dev",
        );
        let m = super::outer_rewrite_map();
        assert_eq!(m.len(), 2);
        for (real, fake) in &m {
            assert_eq!(real.len(), fake.len());
            assert_ne!(real, fake);
            assert!(fake.starts_with("/nix/store/"));
        }
        // Stable under a different $out hash: the placeholder depends on the
        // output NAME only.
        std::env::set_var(
            "out",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-alsa-lib-1.2.16.1",
        );
        assert_eq!(super::outer_rewrite_map()[0].1, m[0].1);
        let data = format!("#define DIR \"{}/share\"\0bin", m[0].0);
        let fwd = super::rewrite_bytes(data.as_bytes(), &m).expect("rewritten");
        assert!(!fwd.windows(m[0].0.len()).any(|w| w == m[0].0.as_bytes()));
        let back: Vec<(String, String)> = m.iter().map(|(r, p)| (p.clone(), r.clone())).collect();
        assert_eq!(super::rewrite_bytes(&fwd, &back).unwrap(), data.as_bytes());
        assert!(super::rewrite_bytes(b"nothing here", &m).is_none());
    }

    #[test]
    fn remove_outer_rpath_strips_the_out_lib_pair() {
        let _env = OUT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("outputs", "out dev");
        std::env::set_var(
            "out",
            "/nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-cc-dropin-1.2.16.1",
        );
        std::env::set_var(
            "dev",
            "/nix/store/npbwm562dyvwjim6j7qa1cish2vxlqr0-alsa-lib-cc-dropin-1.2.16.1-dev",
        );
        let v = "-rpath /nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-cc-dropin-1.2.16.1/lib  -L/nix/store/klkb81wkzlz3bpfv6brnh3gwcapy5b4w-boost-1.89.0/lib";
        assert_eq!(
            super::remove_outer_rpath(v),
            "-L/nix/store/klkb81wkzlz3bpfv6brnh3gwcapy5b4w-boost-1.89.0/lib"
        );
    }

    use super::pool_permits_from_depths;
    use std::collections::HashMap;

    /// Depth 0 is ninja's unbounded. A semaphore built at 0 admits nothing
    /// and hangs every edge in that pool - a wrong limit shows up as a slow
    /// build, this one as no build, and the difference is one comparison.
    #[test]
    fn depth_zero_is_unbounded_and_gets_no_semaphore() {
        let depths: HashMap<String, usize> = [
            ("unbounded".to_string(), 0usize),
            ("link_pool".to_string(), 10),
            ("action_pool".to_string(), 24),
        ]
        .into_iter()
        .collect();
        let permits = pool_permits_from_depths(&depths);
        assert!(
            !permits.contains_key("unbounded"),
            "a depth-0 pool must have no semaphore, or its edges never run"
        );
        assert_eq!(permits.get("link_pool").map(|p| p.cap), Some(10));
        assert_eq!(permits.get("action_pool").map(|p| p.cap), Some(24));
    }

    /// No declared pools is the common case for hand-written build files and
    /// must not error or fabricate one.
    #[test]
    fn no_pools_is_an_empty_map() {
        assert!(pool_permits_from_depths(&HashMap::new()).is_empty());
    }
}

#[cfg(test)]
mod alias_symlink_tests {
    use super::alias_symlink_entry;
    use std::os::unix::fs::symlink;

    // The measured case: meson's configure-time SONAME alias, dangling at
    // scan time (the library links later). The old scan skipped every
    // symlink; the entry must survive precisely because it does not read
    // the target's content.
    #[test]
    fn a_dangling_soname_alias_is_carried_and_an_escape_is_not() {
        let d = std::env::temp_dir().join(format!("nn-alias-test-{}", std::process::id()));
        let orc = d.join("orc");
        std::fs::create_dir_all(&orc).unwrap();

        // Dangling relative alias inside the build dir: carried.
        let link = orc.join("liborc-0.4.so.0");
        symlink("liborc-0.4.so.0.42.0", &link).unwrap();
        assert_eq!(
            alias_symlink_entry(&d, &link),
            Some((
                "orc/liborc-0.4.so.0".to_string(),
                "liborc-0.4.so.0.42.0".to_string()
            ))
        );

        // A `..` that stays inside the build dir: carried.
        let up = orc.join("sibling");
        symlink("../orc/liborc-0.4.so.0.42.0", &up).unwrap();
        assert!(alias_symlink_entry(&d, &up).is_some());

        // Negative controls, each refused for a different reason, because
        // an entry that accepts everything passes the positives above.
        let esc = orc.join("escape");
        symlink("../../etc/passwd", &esc).unwrap();
        assert_eq!(alias_symlink_entry(&d, &esc), None, "escaping target");

        let abs = orc.join("absolute");
        symlink("/nix/store/whatever", &abs).unwrap();
        assert_eq!(alias_symlink_entry(&d, &abs), None, "absolute target");

        let sp = orc.join("has space");
        symlink("target", &sp).unwrap();
        assert_eq!(alias_symlink_entry(&d, &sp), None, "space breaks encoding");

        // Not a symlink at all: refused (read_link fails).
        let plain = orc.join("plain");
        std::fs::write(&plain, b"x").unwrap();
        assert_eq!(alias_symlink_entry(&d, &plain), None, "regular file");

        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod create_symlink_undeclared_output_tests {
    use super::*;

    #[test]
    fn create_symlink_link_is_an_output_with_and_without_cd() {
        let bd = Path::new("/build/source/build-dist");
        // lz4's literal shape: cd into the build dir first.
        let v = undeclared_outputs(
            &[],
            Some("cd /build/source/build-dist && /nix/store/x-cmake/bin/cmake -E create_symlink lz4 lz4cat"),
            bd,
        );
        assert_eq!(v, vec![PathBuf::from("lz4cat")]);
        // No cd prefix: link resolves against the build dir.
        let v = undeclared_outputs(&[], Some("cmake -E create_symlink hello hellocat"), bd);
        assert_eq!(v, vec![PathBuf::from("hellocat")]);
        // A cd elsewhere: link comes back build-dir-relative.
        let v = undeclared_outputs(
            &[],
            Some("cd /build/source/build-dist/sub && cmake -E create_symlink a b"),
            bd,
        );
        assert_eq!(v, vec![PathBuf::from("sub/b")]);
        // Negative control: an unrelated -E command adds nothing.
        let v = undeclared_outputs(&[], Some("cmake -E copy a b"), bd);
        assert!(v.is_empty());
    }

    #[test]
    fn preprocessor_fallback_adds_to_the_scan_rather_than_replacing_it() {
        // gnum4-1.4.21, 2026-08-24. quotearg.c trips the computed-include
        // detector, so the driver fell back to `gcc -MM`. That depfile omits
        // every header gcc marks as a system header, and gnulib writes
        // `#pragma GCC system_header` into the wchar.h it GENERATES INTO THE
        // BUILD DIRECTORY - so the fallback dropped a file that had to be
        // materialized, the task compiled against the system wchar.h, and it
        // died on an implicit declaration of mbszero.
        //
        // The second assertion is the negative control: a "fix" that simply
        // kept `scanned` and discarded the depfile satisfies the first one,
        // so the computed include the preprocessor exists to find has to
        // survive as well, or this test passes on a reintroduced kwsys bug.
        let scanned = vec![
            PathBuf::from("quotearg.c"),
            PathBuf::from("wchar.h"),
            PathBuf::from("limits.h"),
        ];
        let preprocessed = vec![
            PathBuf::from("quotearg.c"),
            PathBuf::from("cmsys/Directory.hxx"),
        ];
        let got = merge_scan_and_preprocessor(scanned, preprocessed);
        assert!(
            got.contains(&PathBuf::from("wchar.h")),
            "the scan's pragma-marked build-dir header was dropped: {got:?}"
        );
        assert!(
            got.contains(&PathBuf::from("cmsys/Directory.hxx")),
            "the preprocessor's computed include was dropped: {got:?}"
        );
        assert_eq!(
            got.iter()
                .filter(|p| *p == &PathBuf::from("quotearg.c"))
                .count(),
            1,
            "the overlap was declared twice: {got:?}"
        );
    }

    /// This test changes the process working directory, which is global.
    static CHDIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // LIVES IN THIS CRATE ON PURPOSE, NOT IN deps-infer WHERE ITS SUBJECT IS.
    // nix-ninja-task's src fileset covers crates/deps-infer, so ANY edit
    // there - a test included - re-keys the task binary, and the task binary
    // is an input of every per-TU derivation. Measured 2026-08-24: adding
    // this test to c_include_parser.rs moved nix-ninja-task's drvPath from
    // 2ai4n9fh to da3j2y88 and would have invalidated all 37,704 banked
    // outputs of the campaign. crates/nix-ninja is NOT in that fileset, and
    // the function under test is pub, so the coverage costs nothing.
    #[test]
    fn angle_include_resolves_against_dash_i_dot() {
        // m4-1.4.21, 2026-08-24, the first REAL failure of the campaign that
        // was not a daemon disconnect. gnulib GENERATES a replacement
        // `wchar.h` into the build directory and declares `mbszero` in it;
        // quotearg.c reaches it as `#include <wchar.h>`, an ANGLE include
        // resolved through `-I.` alone, since automake's DEFAULT_INCLUDES is
        // exactly `-I.`. If that header is not materialized into the task,
        // the angle include silently finds the SYSTEM wchar.h, which has no
        // mbszero, and the compile dies with an implicit declaration - a
        // wrong-header failure wearing a missing-prototype message.
        //
        // The failure direction is what makes it worth a test: a quoted
        // include that is missing fails as "No such file", loudly and at the
        // right place. An angle include that is missing SUCCEEDS at finding
        // the wrong file.
        let _g = CHDIR_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("nn-angle-i-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("wchar.h"), b"#ifndef GEN_WCHAR\n#define GEN_WCHAR\nstatic void mbszero(void*p){(void)p;}\n#endif\n").unwrap();
        std::fs::write(dir.join("config.h"), b"#define PACKAGE \"m4\"\n").unwrap();
        std::fs::write(
            dir.join("quotearg.h"),
            b"#ifndef QA_H\n#define QA_H\n#endif\n",
        )
        .unwrap();
        let src = dir.join("quotearg.c");
        std::fs::write(
            &src,
            b"#include <config.h>\n#include \"quotearg.h\"\n#include <wchar.h>\n",
        )
        .unwrap();

        // `-I.` is resolved against the process working directory, which is
        // what the compile itself does, so the test stands where the
        // compiler would.
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let got = deps_infer::c_include_parser::retrieve_c_includes(
            "gcc -I. -g -O2 -c -o q.o quotearg.c",
            vec![PathBuf::from("quotearg.c")],
            None,
        );
        std::env::set_current_dir(prev).unwrap();
        let got = got.unwrap();
        let names: Vec<String> = got
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(
            names.iter().any(|n| n == "wchar.h"),
            "the generated wchar.h reached only through -I. was not collected: {names:?}"
        );
        // The quoted include is the POSITIVE CONTROL. Without it a walk that
        // collected nothing at all would fail the assertion above for a
        // reason that has nothing to do with angle includes.
        assert!(
            names.iter().any(|n| n == "quotearg.h"),
            "the quoted include was not collected either, so this says nothing \
             about angle includes: {names:?}"
        );
    }
}
