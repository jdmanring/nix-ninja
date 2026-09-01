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
/// One warning per placeholder that will ship unrestored, naming the check
/// that finds it. Separated from the printing so the TEXT can be pinned: a
/// warning whose remedy is wrong is worse than none, and the remedy is the
/// only part a reader acts on.
fn unrestored_warnings(restore: &[(String, String)]) -> Vec<String> {
    restore
        .iter()
        .map(|(placeholder, _real)| {
            format!(
                "nix-ninja: WARNING {placeholder} is not restored in \
                 output-derivation mode; an artifact embedding an output path \
                 ships it dangling. Check with: grep -rl {placeholder} <result>"
            )
        })
        .collect()
}

fn submit_outer_output(
    store_dir: &StoreDir,
    derived_file: &DerivedFile,
    rpc_client: &Arc<BuilderRpcClient>,
) -> Result<()> {
    // THE RESTORE CANNOT RUN HERE, AND SILENCE IS THE PART THAT IS WRONG.
    // A task command naming an outer output path carries a placeholder of
    // the same length, because a per-unit derivation cannot depend on the
    // outer output path without becoming unreusable. Local mode rewrites the
    // placeholder back while materializing the bytes into the build
    // directory. This path never holds those bytes: it clones the final
    // DERIVATION and submits that, and the consumer reads the task output
    // through `builtins.outputOf`, so the artifact it receives is whatever
    // the task wrote, placeholder included.
    //
    // Rewriting to the outer output path would not be a fix either, because
    // that is not the path the consumer resolves. Making the artifact name
    // its own final location is self-referential under content addressing,
    // which is why this needs a design answer rather than a patch.
    //
    // What is fixable is the silence. Say it once, name a file to check, and
    // let a build that cares fail on its own evidence rather than shipping a
    // dangling path nobody looked for.
    // THE INVARIANT THAT MAKES THIS SAFE, ENFORCED RATHER THAN ASSUMED.
    // A placeholder is only written for an output whose value is a store
    // path. Both helpers that drive this mode set `out = "/nonexistent"`,
    // because builder-rpc-v0 leaves the real output unset and genericBuild
    // needs the variable to exist, so the map is empty here and nothing is
    // ever substituted. That is why the restore's absence on this path has
    // no consequence.
    //
    // If the map is NOT empty, this mode is running with a real output path,
    // the restore cannot reach the bytes, and the artifact would ship a path
    // that nothing will create. Refuse instead: no package reaches this, and
    // one that did would otherwise be corrupted silently.
    let restore = crate::task::outer_restore_map();
    if !restore.is_empty() {
        for line in unrestored_warnings(&restore) {
            eprintln!("{line}");
        }
        anyhow::bail!(
            "output-derivation mode with {} real output path(s): the placeholder \
             restore runs only where the driver materializes the bytes, so this \
             configuration would ship an unresolvable path",
            restore.len()
        );
    }

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
mod unrestored_warning_tests {
    use super::unrestored_warnings;

    #[test]
    fn a_warning_names_the_placeholder_and_a_check_that_finds_it() {
        // Nothing to restore, nothing to say. A warning on every build is a
        // warning nobody reads.
        assert!(unrestored_warnings(&[]).is_empty());

        let restore = vec![
            (
                "/nix/store/hjbdq8s5j5dkn2a110sy8zlhfcs4lxsx-gcc-16.2.0".to_string(),
                "/nix/store/cmgbh82ibqygf0f2z5bcz7qr1lqa8sjk-gcc-16.2.0".to_string(),
            ),
            (
                "/nix/store/8xflmr65ja7ydycn7gxzcnipi7l9wip4-gcc-16.2.0-dev".to_string(),
                "/nix/store/f25g2sqsijylwawrixk8zcpkwxqwjyya-gcc-16.2.0-dev".to_string(),
            ),
        ];
        let got = unrestored_warnings(&restore);
        assert_eq!(got.len(), 2, "one line per placeholder");
        for (line, (placeholder, real)) in got.iter().zip(restore.iter()) {
            // THE PLACEHOLDER, NOT THE REAL PATH. Naming the real one would
            // send a reader looking for a path that is present and correct.
            assert!(line.contains(placeholder), "names the placeholder");
            assert!(!line.contains(real.as_str()), "must not name the real path");
            // The remedy is the part acted on, so it is pinned too.
            assert!(
                line.contains(&format!("grep -rl {placeholder}")),
                "carries a check that would find it"
            );
            assert!(line.starts_with("nix-ninja: "), "keeps the log prefix");
        }
    }
}
