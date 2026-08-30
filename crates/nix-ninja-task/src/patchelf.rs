use crate::derived_file::DerivedFile;
use anyhow::{anyhow, Context as _, Result};
use elf::endian::AnyEndian;
use elf::ElfBytes;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn fix_rpaths(store_dir: &Path, outputs: &[DerivedFile]) -> Result<()> {
    for output in outputs {
        let canonical_path = fs::canonicalize(&output.build_path)
            .map_err(|e| anyhow::anyhow!("canonicalize({}): {e}", output.build_path.display()))?;
        if is_elf_dynamic(&canonical_path)? {
            fix_rpath(store_dir, &canonical_path)?;
            println!(
                "nix-ninja-task: Fixed RPATH for {}",
                output.build_path.display()
            );
        }
    }

    Ok(())
}

// Check if this is an executable or shared library that can have RPATH.
// Skip object files (.o) or non-ELF files.
fn is_elf_dynamic(path: &Path) -> Result<bool> {
    // A DIRECTORY OUTPUT IS NOT AN ELF AND READING ONE IS AN ERROR, not a
    // false. Since the driver can declare a whole tree as an output - the
    // syncqt include directory, whose contents ninja never names - this is
    // reached with a directory, and `fs::read` returns EISDIR:
    //     Error: read(.../include/QtSvg): Is a directory
    // which fails the task rather than skipping the entry. Nothing inside an
    // include tree carries an RPATH; a directory output that does would need
    // a walk here, and there is no such output today.
    if path.is_dir() {
        return Ok(false);
    }
    let data = fs::read(path).map_err(|e| anyhow::anyhow!("read({}): {e}", path.display()))?;

    let elf = match ElfBytes::<AnyEndian>::minimal_parse(&data) {
        Ok(elf) => elf,
        Err(_) => return Ok(false), // Not a valid ELF file
    };

    // Only process executables (ET_EXEC) and shared libraries (ET_DYN)
    // Skip object files (ET_REL) as they don't have RPATH
    match elf.ehdr.e_type {
        elf::abi::ET_EXEC | elf::abi::ET_DYN => Ok(true),
        _ => Ok(false),
    }
}

fn fix_rpath(store_dir: &Path, elf_path: &Path) -> Result<()> {
    // One `patchelf --print-rpath` per file. This used to be two: fix_rpath
    // read the raw string to spot a trailing colon and compute_new_rpath read
    // it again through a near-identical helper. This function runs over every
    // output of every task derivation, so the second one was a subprocess per
    // output across a whole distribution.
    let current_rpath = parse_rpath(&get_raw_rpath(elf_path)?);
    if let Some(new_rpath) = compute_new_rpath(store_dir, elf_path, &current_rpath)? {
        apply_rpath(elf_path, &new_rpath)?;
    }
    Ok(())
}

/// Split a raw RPATH on ':', PRESERVING empty entries.
///
/// Faithful on the READ side so callers can see what the binary actually
/// carries. `compute_new_rpath` then drops empties when it rebuilds, and that
/// asymmetry is the whole point.
///
/// An empty element in `DT_RPATH`/`DT_RUNPATH` means the current directory,
/// exactly as it does in `PATH`. In a build tree that is a decision the build
/// made about its own layout. In a STORE OUTPUT it is a search of whatever
/// directory the user happens to be standing in when they run the binary,
/// ahead of the store paths we just resolved - nondeterministic, and a route
/// to loading a library nobody intended.
///
/// This function exists to rewrite build-time paths into store paths, so a
/// build-directory decision is precisely the thing that must not survive it.
/// `f8bb3bd` argued the opposite - that the trailing empty entry was
/// "meaningful" and should be re-appended - and that reasoning was wrong in a
/// way that put a cwd search into every patched output. The original splitter
/// dropped every empty entry, and on the write side it was right to.
fn parse_rpath(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        // "".split(':') yields one empty entry, which would invent an RPATH
        // element for a binary that has none.
        Vec::new()
    } else {
        raw.split(':').map(|s| s.to_string()).collect()
    }
}

fn get_raw_rpath(elf_path: &Path) -> Result<String> {
    let output = Command::new("patchelf")
        .arg("--print-rpath")
        .arg(elf_path)
        .output()
        .map_err(|e| anyhow!("Failed to execute patchelf --print-rpath: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("patchelf --print-rpath failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string())
}

/// The entries of an existing RPATH that survive into the rewritten one.
///
/// Empty entries go, because an empty element means the current directory and
/// this function's output is written into a store binary - see parse_rpath.
/// `$ORIGIN` entries go because the rewrite resolves them to store paths.
///
/// Pulled out of compute_new_rpath so the rule is testable: that function
/// shells out to patchelf against a real ELF, so nothing asserted on its
/// output, which is exactly where the behaviour changed twice this week.
fn retained_entries(current_rpath: &[String]) -> Vec<String> {
    current_rpath
        .iter()
        .filter(|p| !p.is_empty() && !p.contains("$ORIGIN"))
        .cloned()
        .collect()
}

fn compute_new_rpath(
    store_dir: &Path,
    elf_path: &Path,
    current_rpath: &[String],
) -> Result<Option<Vec<String>>> {
    // Resolve RPATH entries with $ORIGIN expansion
    let resolved_rpath = resolve_rpath(current_rpath, elf_path)?;

    // Get needed libraries and collect directories that need to be added to RPATH
    let mut path_added = false;
    let mut new_rpath = retained_entries(current_rpath);

    let needed_libs = get_needed_libs(elf_path)?;

    for lib_name in &needed_libs {
        let Some(lib_path) = resolve_needed(lib_name, &resolved_rpath, store_dir)? else {
            continue;
        };

        let Some(lib_dir) = lib_path.parent() else {
            continue;
        };

        let lib_str = lib_dir
            .to_str()
            .context("Library directiory was not UTF-8")?
            .to_owned();
        if !new_rpath.contains(&lib_str) {
            new_rpath.push(lib_str);
            path_added = true;
        }
    }

    if !path_added {
        Ok(None)
    } else {
        Ok(Some(new_rpath))
    }
}

fn resolve_rpath(rpath: &[String], elf_path: &Path) -> Result<Vec<PathBuf>> {
    let mut resolved_paths = Vec::new();

    let origin = elf_path
        .parent()
        .ok_or_else(|| anyhow!("ELF path has no parent directory"))?
        .to_string_lossy();

    for entry in rpath {
        // An empty entry is the runtime "current directory" marker. It is
        // preserved in the rebuilt RPATH but it is not a directory we can
        // resolve a needed library against here.
        if entry.is_empty() {
            continue;
        }
        let expanded = entry.replace("$ORIGIN", &origin);
        resolved_paths.push(PathBuf::from(expanded));
    }

    Ok(resolved_paths)
}

fn get_needed_libs(elf_path: &Path) -> Result<Vec<String>> {
    let output = Command::new("patchelf")
        .arg("--print-needed")
        .arg(elf_path)
        .output()
        .map_err(|e| anyhow!("Failed to execute patchelf --print-needed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("patchelf --print-needed failed: {stderr}"));
    }

    let needed_str = String::from_utf8_lossy(&output.stdout);
    Ok(needed_str
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

fn resolve_needed(lib_name: &str, rpath: &[PathBuf], store_dir: &Path) -> Result<Option<PathBuf>> {
    // Search for the library in each rpath directory
    for search_dir in rpath {
        let lib_path = search_dir.join(lib_name);

        // If it's already in nix store, return None (don't add to rpath)
        if lib_path.starts_with(store_dir) {
            return Ok(None);
        }

        if lib_path.exists() {
            let canonical_path = fs::canonicalize(&lib_path)
                .map_err(|e| anyhow::anyhow!("canonicalize({}): {e}", lib_path.display()))?;
            return Ok(Some(canonical_path));
        }
    }

    Err(anyhow!("Library {lib_name} not found in RPATH"))
}

fn apply_rpath(elf_path: &Path, new_paths: &[String]) -> Result<()> {
    let rpath_str = new_paths.join(":");
    let mut cmd = Command::new("patchelf");
    cmd.arg("--set-rpath").arg(&rpath_str).arg(elf_path);

    let output = cmd
        .output()
        .map_err(|e| anyhow!("Failed to execute patchelf: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("patchelf failed: {stderr}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_rpath, retained_entries};

    // This file had no tests, which is why f8bb3bd shipped fixing one of the
    // three spellings of its own bug. These are the three.

    #[test]
    fn trailing_empty_entry_survives() {
        assert_eq!(parse_rpath("/a:/b:"), vec!["/a", "/b", ""]);
    }

    #[test]
    fn leading_empty_entry_survives() {
        assert_eq!(parse_rpath(":/a:/b"), vec!["", "/a", "/b"]);
    }

    #[test]
    fn interior_empty_entry_survives_in_place() {
        assert_eq!(parse_rpath("/a::/b"), vec!["/a", "", "/b"]);
    }

    #[test]
    fn no_rpath_is_no_entries_not_one_empty_entry() {
        assert!(parse_rpath("").is_empty());
    }

    #[test]
    fn a_lone_colon_is_two_empty_entries() {
        assert_eq!(parse_rpath(":"), vec!["", ""]);
    }

    // The WRITE side. parse_rpath keeps empty entries so a caller can see
    // them; these assert they never reach a patched binary.

    #[test]
    fn an_empty_entry_never_survives_into_a_store_binary() {
        // ":/a" means "search the current directory, then /a". Preserving
        // that in a store output is a cwd search in somebody else's process.
        let parsed = parse_rpath(":/a:/b");
        assert_eq!(parsed, vec!["", "/a", "/b"], "read side stays faithful");
        assert_eq!(retained_entries(&parsed), vec!["/a", "/b"]);
    }

    #[test]
    fn trailing_and_interior_empties_go_too() {
        assert_eq!(retained_entries(&parse_rpath("/a:/b:")), vec!["/a", "/b"]);
        assert_eq!(retained_entries(&parse_rpath("/a::/b")), vec!["/a", "/b"]);
        assert!(retained_entries(&parse_rpath(":")).is_empty());
    }

    #[test]
    fn origin_entries_are_dropped_because_the_rewrite_resolves_them() {
        let parsed = parse_rpath("$ORIGIN/../lib:/nix/store/a/lib");
        assert_eq!(retained_entries(&parsed), vec!["/nix/store/a/lib"]);
    }

    #[test]
    fn ordinary_rpath_is_unchanged() {
        assert_eq!(
            parse_rpath("/nix/store/a/lib:/nix/store/b/lib"),
            vec!["/nix/store/a/lib", "/nix/store/b/lib"]
        );
    }
}
