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

    /// Do not start new jobs if the load average is greater than N
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
fn resolved_jobs(cli: &Cli) -> usize {
    if cli.jobs == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    } else {
        cli.jobs
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.print_version {
        // For compatibility with meson, it expects >= 1.8.2.
        println!("1.8.2");
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

    let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(resolved_jobs(&cli)))?);
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
    let bytes = rpc_client.clone_drv(final_drv).ok_or_else(|| {
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
            let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(Some(resolved_jobs(&cli)))?);
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
            let bytes = rpc_client.clone_drv(drv_path).ok_or_else(|| {
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

    /// The pool and the semaphore must resolve to the SAME number. They were
    /// two independent bounds and the smaller one won silently, so the check
    /// that matters is the agreement, not either value alone.
    #[test]
    fn resolved_jobs_follows_dash_j() {
        assert_eq!(resolved_jobs(&cli_with(&["-j", "6"])), 6);
        assert_eq!(resolved_jobs(&cli_with(&["-j", "64"])), 64);
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
