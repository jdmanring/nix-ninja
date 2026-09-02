use crate::build::{self, BuildConfig};
use crate::local;
use crate::subtool::compdb;
use crate::subtool::dynamic_task;
use crate::task;
use anyhow::{anyhow, Context as _, Result};
use clap::Parser;
use harmonia_store_derivation::derivation::OutputPathName;
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_path::StoreDir;
use nix_builder_rpc_client::{aterm, BuilderRpcClient};
use nix_ninja_task::derived_file::DerivedFile;
use std::sync::Arc;
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    author,
    disable_version_flag = true,
    about = "nix-ninja: Incremental compilation of Ninja build files via Nix Dynamic Derivations"
)]
pub struct Cli {
    /// Change to DIR before doing anything else
    #[arg(short = 'C')]
    pub dir: Option<PathBuf>,

    /// Specify input build file [default=build.ninja]
    #[arg(short = 'f', default_value = "build.ninja")]
    pub build_filename: PathBuf,

    /// Run a subtool (use '-t list' to list subtools)
    #[arg(short = 't')]
    pub tool: Option<String>,

    /// `-t compdb -x`: expand rspfiles into the emitted command.
    ///
    /// Accepted rather than merely tolerated, because meson passes it. It
    /// sends `-t compdb -x <rules>` to any ninja advertising >= 1.9
    /// (`generate_compdb` in mesonbuild/backend/ninjabackend.py) and we
    /// advertise 1.8.2, so today it is never sent. THE MOMENT THE VERSION
    /// STRING GOES UP THIS FLAG ARRIVES, and until 2026-08-30 clap rejected
    /// it with exit 2 - meson catches that, warns "Could not create
    /// compilation database", and carries on with no file at all. Measured
    /// before the version bump rather than discovered after it.
    #[arg(short = 'x', default_value = "false")]
    pub expand_rspfile: bool,

    /// Run N jobs in parallel (0 means auto: the core count)
    #[arg(short = 'j', default_value = "0", hide = true)]
    pub jobs: usize,

    /// Accepted for ninja compatibility and IGNORED, with a warning.
    ///
    /// Ninja's -l throttles on load average. Nothing here reads it, and that
    /// was silent until 2026-08-20: the field had exactly one occurrence in
    /// the tree, its own declaration. Wiring it would be worse than leaving
    /// it inert, because load average is not a usable signal on this
    /// workload - measured the same day, the machine sat at load 20.6 with
    /// PSI cpu full at 0.00, so the load was entirely uninterruptible I/O
    /// from memory thrash and a load-keyed governor would have been reading
    /// swap pressure through the wrong instrument. Erroring is also wrong:
    /// generators pass -l unprompted and a hard failure would break them.
    /// So it warns, which is the one behaviour that is neither a lie nor a
    /// regression.
    #[arg(short = 'l', default_value = "0.0", hide = true)]
    pub load_average: f64,

    /// Show all command lines while building
    ///
    /// A build tool that accepts this has promised a generator something.
    /// CMake's compiler ABI detection compiles a probe with `-v -Wl,-v` and
    /// PARSES THE BUILD OUTPUT for the link line, which is the only source of
    /// CMAKE_<LANG>_IMPLICIT_LINK_LIBRARIES. Accepted and unread, it recorded
    /// an empty list and reported success, and every Fortran link then failed
    /// for want of -lgfortran.
    ///
    /// A task's command runs inside its own derivation, so its output reaches
    /// no stream of this process, and `builder-rpc-v0`'s success reply carries
    /// a status and a set of realisations rather than the command's output.
    /// An OUTPUT is the channel the derivation model guarantees, so under this
    /// flag the edge declares one more, the task tees its command into it, and
    /// it is materialized and printed with the depfiles. `task.rs` records what
    /// was observed of the daemon's own log, which is the obvious alternative
    /// and did not serve.
    #[arg(short = 'v', long = "verbose", default_value = "false")]
    pub verbose: bool,

    /// Print ninja version
    #[arg(long = "version", default_value = "false")]
    pub print_version: bool,

    /// Specify the Nix store directory
    #[arg(long = "store-dir", default_value = "/nix/store", env = "NIX_STORE")]
    pub store_dir: StoreDir,

    #[arg(long, default_value = "false", env = "NIX_NINJA_DRV", hide = true)]
    pub is_output_derivation: bool,

    /// Target to build (only used with certain subtools)
    #[arg(trailing_var_arg = true)]
    pub targets: Vec<String>,
}

/// The job count `-j` resolves to, with 0 meaning the core count.
///
/// One definition, because it now has two consumers that must agree: the
/// runner's task semaphore and the daemon connection pool. They were
/// independent, and only the semaphore followed `-j`; the pool sat at
/// `available_parallelism() + 1` whatever was asked for, so any `-j` above
/// that silently ran at that number instead.
///
/// -j0 means "auto": the machine's core count. The old reading of 0 as
/// "infinity" is what let one TU's codegen fan-out spawn hundreds of
/// concurrent tasks; unbounded is no longer expressible.
/// Warn once about flags this accepts and does not implement.
///
/// An accepted-and-ignored flag is indistinguishable from a working one from
/// the caller's side, which is how -l survived unnoticed. The warning is the
/// whole fix.
/// `-l` used to be accepted and ignored, which is worse than refusing it: a
/// user who passes a load limit gets no limit and no error. It is honoured
/// now, so the warning says what it actually does instead - load average
/// counts D-state, and a nix-daemon-backed build parks many processes there,
/// so on this workload it reads high while nothing is CPU-starved (measured:
/// load 20.6 at PSI cpu full 0.00). Memory is the control that matters.
fn warn_ignored_flags(cli: &Cli) {
    if cli.load_average != 0.0 {
        eprintln!(
            "nix-ninja: -l {} honoured, but load average counts D-state and this \
             driver parks processes there waiting on the daemon; it reads high while \
             nothing is CPU-starved. Admission is bounded by memory first.",
            cli.load_average,
        );
    }
}

fn resolved_jobs(cli: &Cli) -> usize {
    jobs_from(cli.jobs, std::env::var_os("NIX_BUILD_CORES"))
}

/// `-j`, then `NIX_BUILD_CORES`, then the machine.
///
/// The env value is a PARAMETER so this is testable without mutating the
/// process environment, which is shared and makes a test suite order
/// dependent.
///
/// The middle rung is what makes the caller's concurrency reach this driver at
/// all. A build system sizing a round has two knobs, `max-jobs` and `cores`,
/// and only the first reached us: with no `-j` on the command line the driver
/// took the host's core count whatever the caller asked for, so a round
/// deliberately narrowed to `cores 3` still admitted one task per hardware
/// thread inside every ninja-route package. `nix` sets `NIX_BUILD_CORES` in
/// the derivation for exactly this purpose and every other build system in a
/// sandbox honors it.
///
/// A malformed or zero value falls through to the machine rather than to 1:
/// `NIX_BUILD_CORES=0` is nix's own spelling of "use the core count".
fn jobs_from(dash_j: usize, env_cores: Option<std::ffi::OsString>) -> usize {
    if dash_j != 0 {
        return dash_j;
    }
    if let Some(n) = env_cores
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
}

/// Daemon connections are a DIFFERENT resource from admission slots and must
/// not share a number.
///
/// A connection measured 9.6 GiB of daemon-side memory during round 87 - two
/// of them plus the builds put the machine at PSI full avg10 52.78 with
/// 29.5 GiB swapped. Admission slots are nearly free by comparison, and the
/// whole point of weighting them is to run the shallow strata wide, which
/// wants a budget near the core count. Tying the pool to `-j` means asking
/// for that width opens twenty-four connections and kills the box.
///
/// So `-j` sizes admission and this caps the pool. The ceiling is small on
/// purpose: past a handful of connections the daemon's own per-connection
/// state, not the driver, is what exhausts the machine, and the wedge the
/// watchdog exists for was measured at about twenty concurrent requests.
/// RE-MEASURED 2026-08-21, ROUND 90, TASK 17,500, AND THE 9.6 GiB ABOVE NO
/// LONGER DESCRIBES THIS SYSTEM. Three live daemon workers read 0.44, 0.76
/// and 0.44 GiB RSS; `/nixbuild` held 7.94 GiB of a 24 GiB ceiling, of which
/// `anon` was 1.59 GiB and the rest reclaimable page cache. The machine was
/// 88% idle across 24 cores, load 3.63, with all 26 driver threads in state
/// S - blocked on the daemon, not computing.
///
/// The old figure is kept rather than replaced because it was true when taken.
/// What changed is mechanism, not conditions: round 87 predates NAR streaming,
/// the NAR upload memo and the realise memo, and a connection that buffers
/// whole NARs is a different object from one that streams them. That is what
/// makes 13-21x plausible as a real shift rather than a measurement artifact,
/// though the honest caveat is that 87's number was taken mid-thrash and this
/// one in a steady state, so they are not like-for-like.
///
/// So this ceiling is now calibrated against a defect that has been fixed, and
/// it is very likely the reason the machine is idle while the round crawls.
/// RAISED 3 -> 6 ON 2026-08-21, and the arithmetic is written out because a
/// limit that does not show its terms is exactly what this comment already
/// complains about. Every term re-measured rather than carried forward:
///
///   MemTotal                      30.45 GiB
///   driver peak, round 90          7.70 GiB  (from its own 35 ticks; the
///                                             13.19 GiB this campaign kept
///                                             quoting is an earlier round
///                                             and is stale)
///   daemon worker, per connection  0.80 GiB  (0.44/0.76/0.44 measured,
///                                             worst rounded up)
///   at 6 connections               4.80 GiB
///   driver + workers              12.50 GiB  outside `/nixbuild` entirely
///
/// That leaves about 14 GiB for builds and the desktop against a `/nixbuild`
/// ceiling of 24 GiB, so the machine is over-committed by roughly 10 GiB and
/// WAS BEFORE THIS CHANGE - three more connections cost 2.4 GiB and do not
/// create that gap. It survives because builds rarely reach the ceiling. The
/// missing measurement is peak per-build-cgroup RSS during a round, which is
/// what would say whether 24 GiB can come down without failing a large LTO
/// link; round 91 should record it.
///
/// Six rather than something larger because the machine was 88% idle at three
/// with all 26 driver threads blocked in state S, so doubling is enough to
/// learn whether connections are the bottleneck, and a wrong guess at 6 costs
/// 2.4 GiB rather than 10. Re-measure per connection at this value before
/// raising it again: skipping that requirement is how the old figure went
/// stale in the first place.
const MAX_DAEMON_CONNECTIONS: usize = 6;

fn resolved_connections(cli: &Cli) -> usize {
    resolved_jobs(cli).clamp(1, connection_ceiling(crate::task::available_gib()))
}

/// The pool ceiling for the machine this is actually running on.
///
/// `MAX_DAEMON_CONNECTIONS` was calibrated on ONE machine - 30 GiB, 24
/// threads - against a measured per-connection daemon RSS of 0.44 to 0.76
/// GiB. On a smaller machine that same six is six times a figure the box
/// does not have: at 2 GiB it is the whole machine spent on daemon workers
/// before a compiler starts.
///
/// So the ceiling only ever comes DOWN, and only where no measurement was
/// taken. A machine with the memory the constant assumes gets the constant
/// unchanged, so this is not a retune of a number somebody measured - it is
/// a floor under the machines nobody measured.
///
/// One GiB per connection, from the 0.76 worst case rounded up. Unreadable
/// meminfo answers `u64::MAX` and therefore keeps the ceiling, which is the
/// same fail-open direction `budget_for_memory` takes: a machine whose
/// memory cannot be read must not be silently serialized.
///
/// IT CANNOT SEE ITS SIBLINGS, and that is not fixable here. Every driver in
/// a parallel round reads the same `MemAvailable` and reaches the same
/// answer, so N drivers may each admit what one machine can afford. Sizing
/// the PRODUCT is the consumer's job, through `max-jobs` and `cores`; this
/// only stops one driver being absurd on a small box.
fn connection_ceiling(avail_gib: u64) -> usize {
    let by_memory = usize::try_from(avail_gib.max(1)).unwrap_or(usize::MAX);
    MAX_DAEMON_CONNECTIONS.min(by_memory)
}

#[cfg(test)]
mod connection_ceiling_tests {
    use super::{connection_ceiling, MAX_DAEMON_CONNECTIONS};

    /// THE MACHINE THE CONSTANT WAS MEASURED ON keeps the constant, so this
    /// change is not a retune of anybody's measurement.
    #[test]
    fn a_machine_with_the_assumed_memory_is_unchanged() {
        assert_eq!(connection_ceiling(30), MAX_DAEMON_CONNECTIONS);
        assert_eq!(connection_ceiling(8), MAX_DAEMON_CONNECTIONS);
    }

    /// A SMALL MACHINE COMES DOWN. Six connections at the measured 0.76 GiB
    /// worst case is more than a 2 GiB box has before a compiler starts.
    #[test]
    fn a_small_machine_gets_fewer_connections() {
        assert_eq!(connection_ceiling(2), 2);
        assert_eq!(connection_ceiling(1), 1);
    }

    /// NEVER ZERO. A driver with no connections makes no progress at all,
    /// and a machine reporting no available memory is thrashing rather than
    /// asking to be stopped.
    #[test]
    fn it_never_reaches_zero() {
        assert_eq!(connection_ceiling(0), 1);
    }

    /// FAILS OPEN. `available_gib` answers `u64::MAX` when meminfo cannot be
    /// read, and an unreadable machine must keep the ceiling rather than be
    /// silently serialized.
    #[test]
    fn an_unreadable_meminfo_keeps_the_ceiling() {
        assert_eq!(connection_ceiling(u64::MAX), MAX_DAEMON_CONNECTIONS);
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    warn_ignored_flags(&cli);

    if cli.print_version {
        // A VERSION IS A PROMISE TO ACCEPT WHAT THE GENERATOR THEN EMITS,
        // not a claim about what happens to be implemented. CMake reads this
        // number and decides which constructs to write; below a gate it
        // declines to generate them and we are never asked.
        // `cmGlobalNinjaGenerator.h` carries the table, and 1.10 is five
        // promises:
        //
        //   multiline depfile        n2's escaped_newline_in_depfile
        //   dyndep, for Fortran      dyndep.rs
        //   -t restat                accepted, and accepts absent paths
        //   -t recompact             accepted
        //   multiple outputs         normalized_task_outputs
        //
        // The last was the one nothing had exercised. Driven 2026-08-31 as a
        // two-output edge feeding two compiles: both objects built, from
        // distinct store paths, each defining its own symbol - so the edge
        // produced two files rather than one twice.
        //
        // 1.10 RATHER THAN 1.10.2, deliberately: metadata on regeneration is
        // gated at 1.10.2 and is not implemented. 1.11 wants C++ dyndep and
        // a code page, so this is the highest honest number.
        //
        // BELOW THIS, NO FORTRAN PROJECT USING THE NINJA GENERATOR
        // CONFIGURES AT ALL. CMake refuses outright, naming 1.10, which is
        // what liblapack died on before it reached anything of ours.
        //
        // And it is why `-x` above had to be accepted first: meson sends
        // `-t compdb -x` to anything advertising 1.9 or newer.
        println!("1.10.0");
        return Ok(());
    }

    // Change directory if specified
    if let Some(dir) = &cli.dir {
        std::env::set_current_dir(dir)
            .with_context(|| format!("set_current_dir({})", dir.display()))?;
    }
    let build_dir = std::env::current_dir().context("current_dir")?;

    // Handle subtool if specified
    if let Some(tool) = cli.tool.clone() {
        return subtool(&build_dir, &cli.store_dir, &tool, cli.targets.clone());
    }

    let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(
        resolved_connections(&cli),
    ))?);
    let derived_files = build(&cli, &build_dir, &rpc_client)?;
    if cli.is_output_derivation {
        // One output derivation, by construction: $out is a single path, so
        // several targets have nowhere to go. Refuse rather than silently
        // submitting the first, which is what indexing would have done.
        let [derived_file] = &derived_files[..] else {
            return Err(anyhow!(
                "--is-output-derivation writes a single $out, but {} targets were \
                 requested; name one target",
                derived_files.len()
            ));
        };
        submit_outer_output(&cli.store_dir, derived_file, &rpc_client)?;
    } else {
        local::symlink_derived_files(&rpc_client, &cli.store_dir, &build_dir, &derived_files)?;

        // UPSTREAM #17, STEPS TWO AND THREE. Step one made the depfile a
        // declared content-addressed output of each task; this puts those
        // outputs back into the build directory, which is the only thing
        // standing between the existing read-back and a second run that
        // skips inference entirely.
        //
        // WHY IT WAS NEVER REACHED: `derived_files` above is the requested
        // TARGETS, so local mode materialized the final artifacts and
        // nothing else. Every per-object depfile stayed in the store as an
        // output nobody asked for, `depfile_read_back` found no file on
        // disk, and every run re-scanned from scratch. The read half has
        // been correct and unreachable.
        //
        // Best-effort BY DESIGN, and the polarity is the point: a depfile
        // that fails to materialize costs a scan on the next run, which is
        // the behavior that has always been in force. Failing the build
        // here would turn a cache miss into a build failure, and the
        // read-back guards freshness on its own anyway.
        // THE COMMAND OUTPUT ninja's `-v` promises. It reaches here as a
        // derivation OUTPUT because nothing else crosses the sandbox: the
        // driver sees no stream of a task's command and `builder-rpc-v0`'s
        // success reply carries no command output. An OUTPUT is the channel
        // the derivation model guarantees, so the edge declares one more under
        // `-v`, the task tees its command into it, and it is materialized here
        // with the depfiles.
        //
        // Printed at the end rather than per task, because an output is not
        // realised at the moment its build result arrives. CMake's compiler
        // ABI detection parses the WHOLE captured output of the build command
        // for the link line, so where in that text the transcript sits does
        // not matter to it.
        let verbose_logs = task::take_collected_verbose_logs();
        if !verbose_logs.is_empty() {
            match local::copy_derived_files(&rpc_client, &cli.store_dir, &build_dir, &verbose_logs)
            {
                Ok(_) => {
                    for log in &verbose_logs {
                        let path = build_dir.join(&log.build_path);
                        match std::fs::read(&path) {
                            // STDOUT, which is where ninja puts a command's
                            // output and where a generator looks for it. The
                            // data-channel rule this tree keeps is about the
                            // three subcommands whose stdout is parsed, and a
                            // build invocation is not one of them.
                            Ok(bytes) => print!("{}", String::from_utf8_lossy(&bytes)),
                            Err(e) => eprintln!(
                                "nix-ninja: -v asked for command output and the \
                                 transcript {} could not be read: {e}",
                                path.display()
                            ),
                        }
                        // The transcript is a diagnostic, not a build product,
                        // and leaving it in the build directory would offer a
                        // file the graph never declared to whatever scans that
                        // directory next.
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Err(e) => eprintln!(
                    "nix-ninja: -v asked for command output and {} transcript(s) \
                     could not be materialized ({e}). A generator that parses \
                     this output, CMake's compiler ABI detection among them, \
                     will read an empty string and record an empty result.",
                    verbose_logs.len()
                ),
            }
        }

        let depfiles = task::take_collected_depfiles();
        if !depfiles.is_empty() {
            let n = depfiles.len();
            match local::copy_derived_files(&rpc_client, &cli.store_dir, &build_dir, &depfiles) {
                Ok(copied) => eprintln!(
                    "nix-ninja: collected {copied}/{n} depfile(s) into the build \
                     directory; a rebuild reads them instead of scanning"
                ),
                Err(e) => eprintln!(
                    "nix-ninja: could not collect {n} depfile(s) ({e}); the next \
                     run scans as before"
                ),
            }
        }
    }
    Ok(())
}

/// builder-rpc-v0 requires the submitted path's name to match the caller's
/// `outputPathName`; legacy mode copies the drv into `$out`.
fn submit_outer_output(
    store_dir: &StoreDir,
    derived_file: &DerivedFile,
    rpc_client: &Arc<BuilderRpcClient>,
) -> Result<()> {
    let final_drv = derived_file.derived_path.root_path();
    let final_drv_path = final_drv.to_absolute_path(store_dir);

    let outer_name = env::var("name")
        .map_err(|_| anyhow!("Expected $name to be set inside the outer derivation"))?
        .parse()
        .context("parsing $name as a store path name")?;
    let output_name = OutputName::default();
    let canonical_name = OutputPathName {
        drv_name: &outer_name,
        output_name: &output_name,
    }
    .to_string();
    let bytes = rpc_client.clone_drv(store_dir, final_drv).ok_or_else(|| {
        anyhow!(
            "final drv {} not in uploaded_drvs cache",
            final_drv_path.display()
        )
    })?;
    let renamed = rpc_client
        .add_to_store_text(&canonical_name, &bytes)
        .with_context(|| format!("re-uploading drv as {canonical_name}"))?;
    rpc_client
        .submit_output(&SingleDerivedPath::Opaque(renamed), &OutputName::default())
        .context("submitting outer output")?;
    Ok(())
}

fn build(
    cli: &Cli,
    build_dir: &Path,
    rpc_client: &Arc<BuilderRpcClient>,
) -> Result<Vec<DerivedFile>> {
    let config = BuildConfig {
        build_dir: build_dir.to_path_buf(),
        store_dir: cli.store_dir.clone(),
        is_output_derivation: cli.is_output_derivation,
        // -j0 means "auto": the machine's core count. The old reading of
        // 0 as "infinity" is what let one TU's codegen fan-out spawn
        // hundreds of concurrent tasks; unbounded is no longer
        // expressible.
        jobs: resolved_jobs(cli),
        load_limit: cli.load_average,
        verbose: cli.verbose,
    };

    build::build(
        cli.build_filename
            .to_str()
            .context("Filename was not valid UTF-8")?,
        cli.targets.clone(),
        config,
        rpc_client,
    )
    .with_context(|| {
        format!(
            "building targets {:?} from {}",
            cli.targets,
            cli.build_filename.display()
        )
    })
}

fn subtool(
    build_dir: &Path,
    store_dir: &StoreDir,
    subtool_name: &str,
    targets: Vec<String>,
) -> Result<()> {
    match subtool_name {
        "list" => {
            println!("nix-ninja subtools:");
            println!("  drv           show Nix derivation generated for a target");
            println!("  dynamic-task  generate task derivation from task + discovered deps");
            println!("  compdb        JSON compilation database on stdout");
            println!("  restat        accepted and ignored: no persistent build state");
            println!("  recompact     accepted and ignored: no persistent build state");
            println!("  clean         accepted and ignored: no persistent build state");
            println!("  cleandead     accepted and ignored: no persistent build state");
            Ok(())
        }
        "drv" => {
            let cli = Cli::parse();
            let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(
                resolved_connections(&cli),
            ))?);
            let derived_files = build(&cli, build_dir, &rpc_client)?;
            // `drv` prints ONE derivation. Same reasoning as above: refusing
            // is the only honest answer to several targets here.
            let [derived_file] = &derived_files[..] else {
                return Err(anyhow!(
                    "subtool drv shows a single derivation, but {} targets were \
                     requested; name one target",
                    derived_files.len()
                ));
            };
            let drv_path = derived_file.derived_path.root_path();
            let bytes = rpc_client.clone_drv(store_dir, drv_path).ok_or_else(|| {
                anyhow!(
                    "drv {} not in uploaded_drvs cache",
                    drv_path.to_absolute_path(store_dir).display()
                )
            })?;
            let name = drv_path
                .name()
                .as_ref()
                .strip_suffix(".drv")
                .unwrap_or(drv_path.name().as_ref())
                .parse()
                .context("deriving name from drv store path")?;
            let drv = aterm::parse_derivation_aterm(store_dir, &bytes, name)
                .map_err(|e| anyhow!("parsing drv aterm: {e}"))?;
            // Mimic `nix derivation show`: a JSON object keyed by drv path.
            let shown = serde_json::json!({
                drv_path.to_absolute_path(store_dir).display().to_string(): drv,
            });
            println!("{}", serde_json::to_string_pretty(&shown)?);
            Ok(())
        }
        "dynamic-task" => dynamic_task::run(store_dir, targets),
        // `compdb` WRITES DATA. It shared the no-op arm below until
        // 2026-08-30, which handed every caller an empty
        // compile_commands.json and an exit status of zero.
        "compdb" => {
            let cli = Cli::parse();
            let loader = crate::build::load_file("build.ninja")?;
            compdb::run(&loader.graph, build_dir, &targets, cli.expand_rspfile)
        }
        // These four ARE no-ops here, and for one reason rather than four:
        // this driver keeps no persistent build state. ninja and n2 both
        // carry an on-disk database of mtimes and stale outputs, so for them
        // `restat` marks entries current and `clean`/`cleandead` delete
        // things. The Nix store is our only state, and it is content
        // addressed, so there is nothing local to mark or to sweep. n2 takes
        // the same view of `recompact` - "CMake unconditionally invokes this
        // tool, yuck" (`vendor-n2/src/run.rs`) - and it is added here because
        // its absence is what stopped CMake configuring a Fortran project.
        //
        // `restat` must also ACCEPT PATHS THAT DO NOT EXIST and ignore them.
        // CMake passes them, evmar/n2 #142, and n2's own regression test
        // passes `path_that_does_not_exist` on purpose. Returning Ok here
        // satisfies that by construction; a version of this that resolved its
        // arguments would have to keep it deliberately.
        "restat" | "clean" | "cleandead" | "recompact" => Ok(()),
        _ => {
            anyhow::bail!(
                "Unknown subtool '{subtool_name}'. Use '-t list' to get a list of available subtools."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli_with(args: &[&str]) -> Cli {
        let mut v = vec!["nix-ninja"];
        v.extend_from_slice(args);
        Cli::parse_from(v)
    }

    /// `-j` sizes ADMISSION, and admission is meant to go wide.
    #[test]
    fn resolved_jobs_follows_dash_j() {
        assert_eq!(resolved_jobs(&cli_with(&["-j", "6"])), 6);
        assert_eq!(resolved_jobs(&cli_with(&["-j", "64"])), 64);
    }

    /// Connections are capped INDEPENDENTLY of admission, and this test
    /// replaces one asserting the two agree. They did agree, deliberately,
    /// until a connection was measured at 9.6 GiB of daemon memory: after
    /// that, asking for the wide admission the weighting exists to provide
    /// would have opened one connection per slot and taken the machine down.
    /// Two resources, two numbers - the equality that used to be the
    /// invariant is now the bug.
    #[test]
    fn connections_are_capped_independently_of_admission() {
        // Asserted against the CONSTANT, not against a literal 3. The literal
        // pinned the value in two places, so raising the cap failed a test
        // whose subject is the capping BEHAVIOUR rather than the number - and a
        // test that fails on a deliberate retune teaches people to edit tests.
        // The number itself is defended by the arithmetic at its definition,
        // which a literal here cannot restate and would only duplicate.
        assert_eq!(
            resolved_connections(&cli_with(&["-j", "24"])),
            MAX_DAEMON_CONNECTIONS
        );
        assert_eq!(
            resolved_connections(&cli_with(&["-j", "64"])),
            MAX_DAEMON_CONNECTIONS
        );
        // Below the cap the request still wins: a deliberate -j1 must not be
        // silently widened to the full connection pool.
        assert_eq!(resolved_connections(&cli_with(&["-j", "1"])), 1);
        assert_eq!(resolved_connections(&cli_with(&["-j", "2"])), 2);
        // AND THE CAP MUST ACTUALLY BIND, which the two assertions above stop
        // proving once they read the constant: with a bug that ignored the cap
        // entirely, `resolved_connections(-j 64)` would return 64 and the
        // comparison would still be against MAX_DAEMON_CONNECTIONS only if the
        // constant were 64. This pins the relationship instead.
        assert!(
            resolved_connections(&cli_with(&["-j", "64"])) < 64,
            "a -j far above the cap must be capped, not passed through"
        );
        // And never zero, which would be a pool that can never serve.
        assert!(resolved_connections(&cli_with(&["-j", "0"])) >= 1);
    }

    /// `-j 0` means "auto", never "unbounded". An unbounded runner is what
    /// spawned hundreds of concurrent tasks off one TU's codegen fan-out.
    ///
    /// ASSERTED WITHOUT NAMING THE NUMBER, because what "auto" resolves to now
    /// depends on `NIX_BUILD_CORES`. This test read the machine's core count as
    /// a literal and passed everywhere until it ran INSIDE a derivation, where
    /// nix sets that variable and the expected 24 arrived as 4. It failed the
    /// driver's own build and took a round with it. The machine fallback is
    /// pinned in `jobs_prefers_dash_j_then_env_then_machine`, which passes the
    /// environment rather than reading it.
    #[test]
    fn resolved_jobs_zero_means_auto_not_infinity() {
        let auto = resolved_jobs(&cli_with(&["-j", "0"]));
        assert!(auto > 0, "auto must resolve to a real bound");
        // The default is 0, so a bare invocation takes the same path.
        assert_eq!(resolved_jobs(&cli_with(&[])), auto);
        // Whatever auto resolves to, an explicit -j still wins over it, which
        // is what says 0 is a VALUE here rather than a missing argument.
        assert_eq!(resolved_jobs(&cli_with(&["-j", "7"])), 7);
    }

    /// `-j` beats the environment, the environment beats the machine.
    ///
    /// The env value is passed rather than set, so these cases say nothing
    /// about whatever `NIX_BUILD_CORES` happens to hold in the shell that runs
    /// the suite - a test reading an ambient tunable asserts about the code AND
    /// the environment, and fails against correct behavior.
    #[test]
    fn jobs_prefers_dash_j_then_env_then_machine() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        let machine = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);

        // An explicit -j wins over the environment. A caller who typed a
        // number must not be silently retuned by the sandbox.
        assert_eq!(jobs_from(6, os("3")), 6);

        // With no -j, the caller's `cores` reaches the driver. This is the
        // rung that did not exist.
        assert_eq!(jobs_from(0, os("3")), 3);
        assert_eq!(jobs_from(0, os(" 12 ")), 12);

        // Absent, zero, or malformed falls to the machine. Zero is nix's own
        // spelling of "use the core count", so it must not become 1.
        assert_eq!(jobs_from(0, None), machine);
        assert_eq!(jobs_from(0, os("0")), machine);
        assert_eq!(jobs_from(0, os("")), machine);
        assert_eq!(jobs_from(0, os("many")), machine);
        assert_eq!(jobs_from(0, os("-4")), machine);

        // And never zero, whatever the input: a runner with no slots stalls.
        for v in [None, os("0"), os("x")] {
            assert!(jobs_from(0, v) > 0);
        }
    }
}

#[cfg(test)]
mod ignored_flag_tests {
    use super::*;
    use clap::Parser;

    /// -l parses and is readable. The defect it guards is not a parse
    /// failure but a SILENT one: the field had a single occurrence in the
    /// whole tree, its own declaration, so every caller passing it got the
    /// ninja behaviour it names and none of it.
    #[test]
    fn dash_l_parses_and_is_not_silently_dropped() {
        let cli = Cli::parse_from(["nix-ninja", "-l", "4.5"]);
        assert_eq!(cli.load_average, 4.5);
        // default stays 0.0 so warn_ignored_flags says nothing unprompted
        assert_eq!(Cli::parse_from(["nix-ninja"]).load_average, 0.0);
    }
}
