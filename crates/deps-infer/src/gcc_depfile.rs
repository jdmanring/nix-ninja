use crate::gcc_depfile_parser::{spawn_gcc_generate_depfile, DepsConfig};
use anyhow::{anyhow, Result};
use n2::scanner;
use std::path::{Path, PathBuf};

pub fn retrieve_c_includes(cmdline: &str) -> Result<Vec<PathBuf>> {
    // A FIXED /tmp NAME IS TWO BUGS AND ONE OF THEM IS SILENT. The driver
    // runs this from many threads at once, so a shared name means one TU
    // reads another's depfile and declares the wrong headers - wrong
    // inputs, not a crash. It is also a predictable path in a
    // world-writable directory. Unique per call, removed on the way out.
    let unique = format!(
        "nix-ninja-deps-{}-{:?}.d",
        std::process::id(),
        std::thread::current().id()
    );
    let owned = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
    let depfile_path: &Path = &owned;
    let _cleanup = DepfileGuard(owned.clone());

    spawn_gcc_generate_depfile(
        cmdline,
        &DepsConfig {
            output_path: depfile_path.into(),
            include_system_headers: false,
        },
    )?;

    let buf = scanner::read_file_with_nul(depfile_path)?;
    let mut scanner = scanner::Scanner::new(&buf);

    let depfile = n2::depfile::parse(&mut scanner)
        .map_err(|err| anyhow!(scanner.format_parse_error(depfile_path, err)))?;

    let mut deps: Vec<PathBuf> = Vec::new();
    for (_, values) in depfile.iter() {
        for value in values {
            deps.push(value.into());
        }
    }

    Ok(deps)
}

/// Removes the depfile however this function leaves - the early `?` returns
/// are the paths that would otherwise litter /tmp for the run's lifetime.
struct DepfileGuard(PathBuf);

impl Drop for DepfileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
