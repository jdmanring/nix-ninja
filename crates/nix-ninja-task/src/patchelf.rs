use crate::derived_file::DerivedFile;
use anyhow::{anyhow, Context as _, Result};
use elf::endian::AnyEndian;
use elf::ElfBytes;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn fix_rpaths(store_dir: &Path, outputs: &[DerivedFile]) -> Result<()> {
    // ONE LINE PER TASK, not per output. This printed a line for every ELF
    // it touched, and a task whose output is a tree (Qt's syncqt, cmake
    // custom targets) has hundreds - so the log of a link-heavy target was
    // mostly this message. The count is the part worth reading: it says how
    // many of the task's outputs were dynamic ELF at all.
    let mut fixed = 0usize;
    for output in outputs {
        let canonical_path = fs::canonicalize(&output.build_path)
            .map_err(|e| anyhow::anyhow!("canonicalize({}): {e}", output.build_path.display()))?;
        if is_elf_dynamic(&canonical_path)? {
            fix_rpath(store_dir, &canonical_path)?;
            fixed += 1;
        }
    }
    if fixed > 0 {
        println!(
            "nix-ninja-task: fixed RPATH on {fixed} of {} output(s)",
            outputs.len()
        );
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
/// An empty element in `DT_RPATH`/`DT_RUNPATH` means the current directory,
/// exactly as it does in `PATH`, and for a long time this crate read that as
/// reason enough to drop it when rebuilding. See `assemble_rpath` for why a
/// run of them is CMake's space reservation rather than a library search, and
/// why removing them broke the install of every CMake package whose install
/// RPATH is longer than its build-tree one.
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

/// The RPATH a rewritten output should carry, from what it had and the store
/// directories its NEEDED libraries resolved to. `None` means nothing was
/// added and the binary should be left alone.
///
/// PURE, AND SEPARATE FROM THE ELF READING, so the rule can be pinned where
/// it is actually applied: `compute_new_rpath` shells out to patchelf against
/// a real ELF, and for as long as the retention rule lived in a helper of its
/// own, nine tests covered the helper and nothing covered the fact that this
/// function called it.
///
/// THE FILTER THAT USED TO STAND HERE IS THE DEFECT NOW, which is the reverse
/// of what this comment said for a week. It dropped empty entries to keep a
/// current-directory search out of a store output, and empty entries are also
/// how CMake reserves space for its own install-time rewrite.
pub fn assemble_rpath(current_rpath: &[String], needed_dirs: &[String]) -> Option<Vec<String>> {
    // EVERY ENTRY THE BINARY CARRIED SURVIVES, EMPTY ONES INCLUDED, and the
    // empty ones are the reason this is not a filter.
    //
    // CMake reserves room for its install-time rewrite by padding the
    // build-time RPATH with a run of EMPTY entries sized to the install
    // value, and `file(RPATH_CHANGE)` records the padding as part of
    // `OLD_RPATH`. It edits the string in place, so that reservation IS the
    // allocation: drop the padding and the install refuses, either because
    // the recorded sequence is no longer in the file or because the
    // replacement no longer fits. Measured, one CMake project, three arms:
    //
    //     untouched                          install succeeds
    //     padding stripped, store dir added   RPATH_CHANGE refuses
    //     padding kept,     store dir added   install succeeds
    //
    // A dropped run of colons took openexr and libjpeg-turbo out of a
    // distribution round, and CMake sizes the reservation to the byte - 78
    // reserved against 77 needed in openexr's case - so there is no slack to
    // spend anywhere.
    //
    // What that costs: an empty entry means the current directory, and this
    // output goes to the store, so a binary CMake never rewrites ships a cwd
    // library search. That case is not silent - such a binary also keeps its
    // build-tree entry, which is what nixpkgs' forbidden-reference audit
    // refuses - so the failure is loud and names the package. A predicate
    // telling CMake's padding from a build's own empty entry is the third
    // rule over this one string, and the previous two both shipped defects.
    let mut new_rpath = current_rpath.to_vec();
    let mut path_added = false;
    for lib_str in needed_dirs {
        if !new_rpath.contains(lib_str) {
            new_rpath.push(lib_str.clone());
            path_added = true;
        }
    }
    if path_added {
        Some(new_rpath)
    } else {
        None
    }
}

fn compute_new_rpath(
    store_dir: &Path,
    elf_path: &Path,
    current_rpath: &[String],
) -> Result<Option<Vec<String>>> {
    // Resolve RPATH entries with $ORIGIN expansion
    let resolved_rpath = resolve_rpath(current_rpath, elf_path)?;

    let needed_libs = get_needed_libs(elf_path)?;
    let mut needed_dirs: Vec<String> = Vec::new();

    for lib_name in &needed_libs {
        let Some(lib_path) = resolve_needed(lib_name, &resolved_rpath, store_dir)? else {
            continue;
        };

        let Some(lib_dir) = lib_path.parent() else {
            continue;
        };

        needed_dirs.push(
            lib_dir
                .to_str()
                .context("Library directiory was not UTF-8")?
                .to_owned(),
        );
    }

    Ok(assemble_rpath(current_rpath, &needed_dirs))
}

fn resolve_rpath(rpath: &[String], elf_path: &Path) -> Result<Vec<PathBuf>> {
    let mut resolved_paths = Vec::new();

    let origin = elf_path
        .parent()
        .ok_or_else(|| anyhow!("ELF path has no parent directory"))?
        .to_string_lossy();

    for entry in rpath {
        // An empty entry is the runtime marker for the current directory,
        // and CMake's padding is a run of them. Either way it is not a
        // directory a needed library can be resolved against.
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
        //
        // PRE-EXISTING and untouched by this week's changes, flagged during
        // the 2026-08-29 audit so it is on the record rather than rediscovered:
        // this returns on the FIRST rpath entry that is store-prefixed,
        // without checking the library actually exists there. For an rpath of
        // `/nix/store/a/lib:/build/out/lib`, a library that lives only in the
        // second entry is skipped. Not fixed here because nothing has been
        // observed failing on it and a speculative change to rpath resolution
        // is how this file acquired its last two defects.
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
    use super::{assemble_rpath, parse_rpath};

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

    // The WRITE side. Every entry survives, and each of these records WHY,
    // because two of them assert the opposite of what this file asserted
    // before and a bare expectation leaves the next reader unable to tell a
    // deliberate reversal from a regression.

    /// CMAKE'S PADDING, AND THE CASE THAT TOOK TWO PACKAGES OUT OF A ROUND.
    /// The run of empty entries is sized to the install-time value and
    /// `file(RPATH_CHANGE)` records it as part of `OLD_RPATH`; dropping it
    /// removes the allocation the rewrite is edited into.
    #[test]
    fn cmake_padding_survives_because_it_is_the_space_the_install_rewrite_needs() {
        let parsed = parse_rpath("/build/source/build/src/lib/Iex:::::");
        let got = assemble_rpath(&parsed, &["/nix/store/a/lib".to_string()]).unwrap();
        assert_eq!(
            got,
            vec![
                "/build/source/build/src/lib/Iex",
                "",
                "",
                "",
                "",
                "",
                "/nix/store/a/lib"
            ],
            "the padding is CMake's byte reservation, not a library search"
        );
        // The appended directory goes AFTER the padding, so the recorded
        // sequence is still present with a colon after it - which is the
        // boundary CMake requires.
        let raw = got.join(":");
        assert!(raw.starts_with("/build/source/build/src/lib/Iex:::::"));
    }

    /// The cost of the rule above, stated rather than hidden. An empty entry
    /// means the current directory, so a binary CMake never rewrites carries
    /// a cwd search into the store. It is accepted because such a binary also
    /// keeps its build-tree entry, which nixpkgs' own audit refuses by name.
    #[test]
    fn a_lone_empty_entry_survives_too_and_that_is_the_accepted_cost() {
        let parsed = parse_rpath(":/a:/b");
        assert_eq!(
            assemble_rpath(&parsed, &["/nix/store/a/lib".to_string()]).unwrap(),
            vec!["", "/a", "/b", "/nix/store/a/lib"]
        );
    }

    /// `$ORIGIN` STAYS, and for a different reason than the empties: it names
    /// the binary's own directory, which in a store output is the store path,
    /// and it is what CMake keeps as its record. abseil-cpp could not install
    /// a library we had rewritten because `file(RPATH_CHANGE)` looks for
    /// `$ORIGIN` as an entry sequence and refuses when it is gone.
    #[test]
    fn origin_survives_because_cmake_records_it_and_it_is_safe_in_a_store_path() {
        let parsed = parse_rpath("$ORIGIN/../lib:/nix/store/a/lib");
        assert_eq!(
            assemble_rpath(&parsed, &["/nix/store/b/lib".to_string()]).unwrap(),
            vec!["$ORIGIN/../lib", "/nix/store/a/lib", "/nix/store/b/lib"]
        );
    }

    /// Nothing to add means the binary is left alone rather than rewritten
    /// with the same entries, which is what keeps a needless patchelf run off
    /// every output of every task.
    #[test]
    fn nothing_new_means_no_rewrite() {
        let parsed = parse_rpath("/nix/store/a/lib:");
        assert_eq!(
            assemble_rpath(&parsed, &["/nix/store/a/lib".to_string()]),
            None
        );
    }

    #[test]
    fn ordinary_rpath_is_unchanged() {
        assert_eq!(
            parse_rpath("/nix/store/a/lib:/nix/store/b/lib"),
            vec!["/nix/store/a/lib", "/nix/store/b/lib"]
        );
    }
}
