use crate::build::{self, BuildConfig};
use crate::local;
use crate::subtool::compdb;
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

    /// `-t compdb -x`: expand rspfiles into the emitted command.
    ///
    /// Accepted rather than merely tolerated, because meson passes it. It
    /// sends `-t compdb -x <rules>` to any ninja advertising >= 1.9
    /// (`generate_compdb` in mesonbuild/backend/ninjabackend.py), and
    /// `--version` here reports 1.8.2, so today it is never sent. The moment
    /// that string goes up this flag arrives, and clap rejects an unknown
    /// flag with exit 2 - which meson catches, warns "Could not create
    /// compilation database" about, and carries on from with no file at all.

    #[arg(short = 'x', default_value = "false")]
    pub expand_rspfile: bool,

    /// Run N jobs in parallel (0 means infinity)
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

    let rpc_client = Arc::new(BuilderRpcClient::connect_from_env()?);
    let derived_file = build(&cli, &build_dir, &rpc_client)?;
    if cli.is_output_derivation {
        submit_outer_output(&cli.store_dir, &derived_file, &rpc_client)?;
    } else {
        local::symlink_derived_files(&rpc_client, &cli.store_dir, &build_dir, &[derived_file])?;
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

fn build(cli: &Cli, build_dir: &Path, rpc_client: &Arc<BuilderRpcClient>) -> Result<DerivedFile> {
    let config = BuildConfig {
        build_dir: build_dir.to_path_buf(),
        store_dir: cli.store_dir.clone(),
        is_output_derivation: cli.is_output_derivation,
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
            let rpc_client = Arc::new(BuilderRpcClient::connect_from_env()?);
            let derived_file = build(&cli, build_dir, &rpc_client)?;
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
