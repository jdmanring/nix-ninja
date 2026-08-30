use anyhow::Context;
use anyhow::{anyhow, Result};
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use harmonia_store_derivation::placeholder::StorePathOrPlaceholder;
use harmonia_store_path::StoreDir;
use harmonia_store_path::StorePath;
use std::fmt;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

/// Represents a file input or output for nix-ninja-task builds.
///
/// DerivedFile describes how files are arranged in the build directory that nix-ninja-task
/// creates. The build directory contains symlinks that recreate the original source structure,
/// allowing builds to reference files using relative paths while the actual files come from
/// various Nix store locations.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DerivedFile {
    pub derived_path: SingleDerivedPath,
    pub build_path: PathBuf, // Where file appears in build dir (symlink destination)
    pub rel_path: Option<PathBuf>, // Where file appears within derived path (None for opaque)
}

impl DerivedFile {
    /// Encodes this DerivedFile for passing from nix-ninja to nix-ninja-task.
    ///
    /// Format: `"<path_or_placeholder>:<build_path>:<rel_path>"`
    ///
    /// where `<path>` is *without* the store dir. (That is known from context.)
    pub fn to_encoded(&self, store_dir: &StoreDir) -> String {
        let path_str = store_dir
            .display(&StorePathOrPlaceholder::from(&self.derived_path))
            .to_string();
        let rel_path_str = self
            .rel_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        format!(
            "{}:{}:{}",
            path_str,
            self.build_path.to_string_lossy(),
            rel_path_str
        )
    }

    /// Decodes a DerivedFile from the string format created by `to_encoded()`.
    /// Used by nix-ninja-task to recreate build directory symlinks.
    pub fn from_encoded(store_dir: &StoreDir, encoded: &str) -> Result<Self> {
        let mut parts = encoded.split(':');
        let store_path_str = parts
            .next()
            .ok_or_else(|| anyhow!("Missing store path in encoded derived file: {encoded}"))?;
        let store_path: StorePath = store_dir
            .parse(store_path_str)
            .context("Parsing encoded store path")?;
        let derived_path = SingleDerivedPath::Opaque(store_path);
        let build_path = PathBuf::from(
            parts
                .next()
                .ok_or_else(|| anyhow!("Missing build path in encoded derived file: {encoded}"))?,
        );
        let rel_path = parts.next().filter(|s| !s.is_empty()).map(PathBuf::from);

        Ok(DerivedFile {
            derived_path,
            build_path,
            rel_path,
        })
    }

    pub fn absolute_path(&self, store_dir: &StoreDir) -> PathBuf {
        let base_path = PathBuf::from(
            store_dir
                .display(&StorePathOrPlaceholder::from(&self.derived_path))
                .to_string(),
        );
        if let Some(rel_path) = &self.rel_path {
            base_path.join(rel_path)
        } else {
            base_path
        }
    }
}

impl fmt::Display for DerivedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base_path = match StorePathOrPlaceholder::from(&self.derived_path) {
            StorePathOrPlaceholder::StorePath(store_path) => {
                PathBuf::from(store_path.to_base_path())
            }
            StorePathOrPlaceholder::Placeholder(placeholder) => placeholder.render(),
        };
        if let Some(rel_path) = &self.rel_path {
            write!(f, "{:?}", base_path.join(rel_path))
        } else {
            write!(f, "{:?}", base_path)
        }
    }
}

/// Creates symlinks for derived files under the specified prefix.
///
/// For each derived file, creates a symlink at `prefix/${derived_file.build_path}`
/// pointing to the actual file at `derived_file.rel_path`.
/// Link every file under `src` into `dst`, creating real directories on the
/// way. Existing entries are left alone rather than replaced: a declared
/// output already materialised at that path is the same file this would link.
fn link_tree(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| {
        anyhow!(
            "create_dir_all({}) for a directory input: {e}",
            dst.display()
        )
    })?;
    for entry in fs::read_dir(src)
        .map_err(|e| anyhow!("read_dir({}) for a directory input: {e}", src.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let md = match fs::metadata(&from) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            link_tree(&from, &to)?;
        } else if !to.exists() && !to.is_symlink() {
            std::os::unix::fs::symlink(&from, &to)
                .map_err(|e| anyhow!("symlink({} -> {}): {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

pub fn create_symlinks(
    prefix: &std::path::Path,
    store_dir: &StoreDir,
    inputs: Vec<DerivedFile>,
    overwrite: bool,
) -> Result<()> {
    // DIRECTORY INPUTS LAST. A tree and an individually declared file can
    // name the same build path from the same rule - the tree output carries
    // a copy of every public forwarding header, and each is also its own
    // output. Whichever is linked first wins, and the second is then a
    // CONFLICT by the check below: same content, different store path.
    // The individually declared file is the authoritative one, so the tree
    // goes last and fills only what is missing, which is what link_tree
    // already does.
    let mut inputs = inputs;
    inputs.sort_by_key(|i| i.absolute_path(store_dir).is_dir());
    for input in inputs {
        let source_path = input.absolute_path(store_dir);
        let dest_path = prefix.join(&input.build_path);

        // Create parent directories if they don't exist. An ABSOLUTE
        // build_path silently discards `prefix` in the join above (Rust
        // Path::join semantics) and escapes the build dir, so name the
        // path in the error - a bare EACCES here cost a diagnosis cycle.
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "create_dir_all({}) for input {}: {e}",
                    parent.display(),
                    input.build_path.display()
                )
            })?;
        }

        if !source_path.exists() {
            return Err(anyhow!(
                "nix-ninja-task: symlink source does not exist: {:?}",
                source_path
            ));
        }

        // A TREE OUTPUT IS NEVER REMOVED WHOLE: it is linked file by file
        // below, over whatever the directory already holds (in the outer
        // build dir that is CMake's configure-time content), and the
        // per-file links inside overwrite on their own. `remove_file` on
        // it is EISDIR (materialize-all, qtsvg, 2026-08-23).
        if overwrite && dest_path.exists() && !source_path.is_dir() {
            fs::remove_file(&dest_path)
                .map_err(|e| anyhow::anyhow!("remove_file({}): {e}", dest_path.display()))?;
        }

        // TWO INPUTS CAN NAME ONE DESTINATION, and when they name the same
        // SOURCE that is a duplicate rather than a conflict. The driver keys
        // its input set by the build path as SPELLED, so one file discovered
        // by two routes arrives twice - once relative to the build dir and
        // once as a `../` climb that resolves back to it. Measured on qtsvg
        // 6.11.1, 2026-08-22: `src/svg/.../Qt6SvgPrivateTargets.cmake` and
        // `../../../build/src/build/src/svg/.../Qt6SvgPrivateTargets.cmake`,
        // same store path, same destination.
        // The second symlink then fails EEXIST, and the message names a real
        // store path and a real destination, so it reads as a collision
        // between two different files.
        // Skip when the link already points where this one would; refuse
        // when it points somewhere else, because that IS a conflict and
        // silently keeping the first would be a wrong build with no symptom.
        // A DIRECTORY INPUT IS LINKED FILE BY FILE, NOT AS A DIRECTORY.
        // The syncqt include tree arrives as one store path, and symlinking
        // the directory itself makes it read-only: the rule's individually
        // declared public headers then have nowhere to land, and any task
        // that writes into the tree dies EACCES inside the store. Linking the
        // CONTENTS leaves the directory a real, writable one and lets the
        // per-file links for the declared outputs sit beside the undeclared
        // ones. First writer wins, and a file already present is left alone -
        // it is the same content by construction, since both spellings came
        // from the same task.
        if source_path.is_dir() {
            link_tree(&source_path, &dest_path)?;
            continue;
        }

        if dest_path.is_symlink() {
            match fs::read_link(&dest_path) {
                Ok(existing) if existing == source_path => continue,
                Ok(existing) => {
                    return Err(anyhow!(
                        "nix-ninja-task: {} is already a symlink to {}, and a \
                         second input wants it to point at {}. Two different \
                         files claim one build path.",
                        dest_path.display(),
                        existing.display(),
                        source_path.display()
                    ))
                }
                Err(e) => {
                    return Err(anyhow!(
                        "nix-ninja-task: read_link({}): {e}",
                        dest_path.display()
                    ))
                }
            }
        }

        // Python scripts are COPIED, not symlinked: a script that takes
        // realpath(__file__) - Chromium's version.py does, to find its
        // sibling LASTCHANGE.dummy - resolves a symlink into the store,
        // where its data siblings do not exist. A copy keeps __file__
        // inside the sandbox tree, where the siblings' own symlinks sit
        // beside it. (PYTHONPATH covers .py IMPORT siblings; this covers
        // data-file siblings, which no import path can redirect.)
        // node_modules gets the same treatment for the same reason: node
        // realpaths a required script (typescript's bin/tsc is a
        // shebang file with no extension), and its own relative
        // require('../lib/tsc.js') then resolves inside the store,
        // where the module tree does not exist.
        // .mjs/.js/.cjs get the same treatment: node realpaths a module
        // before resolving its relative imports, so a symlinked
        // eslint.config.mjs resolved ../../third_party/... from inside
        // /nix/store and landed on /third_party (measured).
        if input
            .build_path
            .extension()
            .is_some_and(|e| e == "py" || e == "mjs" || e == "js" || e == "cjs")
            || input
                .build_path
                .components()
                .any(|c| c.as_os_str() == "node_modules")
        {
            fs::copy(&source_path, &dest_path)
                .map_err(|e| anyhow!("copy({:?} -> {}): {e}", source_path, dest_path.display()))?;
            continue;
        }

        symlink(&source_path, &dest_path).map_err(|e| {
            anyhow!(
                "Failed to create symlink from {:?} to {}: {}",
                source_path,
                dest_path.display(),
                e
            )
        })?;
    }

    Ok(())
}
