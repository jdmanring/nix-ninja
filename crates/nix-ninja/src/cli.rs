use crate::build::{self, BuildConfig};
use crate::local;
use crate::subtool::dynamic_task;
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
    if cli.jobs == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    } else {
        cli.jobs
    }
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
    resolved_jobs(cli).min(MAX_DAEMON_CONNECTIONS).max(1)
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    warn_ignored_flags(&cli);

    if cli.print_version {
        // For compatibility with meson, it expects >= 1.8.2.
        println!("1.8.2");
        return Ok(());
    }

    // Said out loud at startup rather than left to be inferred from the
    // counters: a round that PRUNED inputs and a round that only measured
    // them produce different artifacts, and telling them apart afterwards
    // from a log that never named the mode is guesswork.
    if crate::task::init_prune_inputs() {
        eprintln!(
            "nix-ninja: NIX_NINJA_PRUNE_INPUTS is set - declaring only headers \
             the last compile of each edge actually read. Under-declaration \
             fails inside the sandbox; this is safe only under a daemon that \
             sandboxes."
        );
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

    let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(resolved_connections(&cli)))?);
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

fn build(cli: &Cli, build_dir: &Path, rpc_client: &Arc<BuilderRpcClient>) -> Result<Vec<DerivedFile>> {
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
            Ok(())
        }
        "drv" => {
            let cli = Cli::parse();
            let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(resolved_connections(&cli)))?);
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
        // Meson compatibility tools.
        "restat" | "clean" | "cleandead" | "compdb" => {
            // TODO: Implement what's necessary, I think only compdb needs to
            // work and the rest can no-op.
            Ok(())
        }
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

    #[test]
    fn resolved_jobs_zero_means_cores_not_infinity() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        assert_eq!(resolved_jobs(&cli_with(&["-j", "0"])), cores);
        // The default is 0, so a bare invocation takes the same path. An
        // unbounded runner is what spawned hundreds of concurrent tasks off
        // one TU's codegen fan-out.
        assert_eq!(resolved_jobs(&cli_with(&[])), cores);
        assert!(resolved_jobs(&cli_with(&[])) > 0);
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

