use anyhow::Context;
use anyhow::{anyhow, Result};
// The dynamic section is read directly rather than through `patchelf`, so the
// predicate needs none of the task PATH; see `origin_relative_executable`.
use elf::endian::AnyEndian;
use elf::ElfBytes;
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use harmonia_store_derivation::placeholder::StorePathOrPlaceholder;
use harmonia_store_path::StoreDir;
use harmonia_store_path::StorePath;
use std::fmt;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

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

/// The separator between ENTRIES of an encoded list.
///
/// `NIX_NINJA_INPUTS`, `NIX_NINJA_OUTPUTS`, `NIX_NINJA_ALIASES` and
/// `NIX_NINJA_MAKE_DIRS` carry one entry per element and EVERY element
/// contains a build path. Joined on a space, a build path containing a space
/// split one entry into two: meson names a target directory after its target,
/// so libepoxy 1.5.10 has `test/khronos typedefs.p`, and the tail arrived at
/// `from_encoded` alone and failed as `non-absolute store path
/// "typedefs.p/khronos_typedefs.c.o"`. That message names the store-path parse
/// and the split happened one layer above it. Two objects took the package and
/// a whole server edition with them.
///
/// A newline cannot occur in a path ninja can express, where a space routinely
/// does. THE SHIM'S ESCAPING FIX IS WHAT EXPOSED THIS: while the build
/// statement was malformed ninja split the path first and the driver never saw
/// a name with a space in it, so the defect was recorded as the shim's and was
/// correctly recorded at the time.
///
/// The FIELD separator inside one entry is still `:`, and a build path
/// containing a colon would mis-split the three fields the same way. No such
/// path has been observed. DEFER(a build path carrying a colon is reported):
/// length-prefix the fields or escape the separator.
pub const ENCODED_LIST_SEP: &str = "\n";

/// Split a list written with [`ENCODED_LIST_SEP`], dropping empty entries.
///
/// The empty filter is load bearing rather than tidiness. A value reaches the
/// task either inline or through nix's `passAsFile`, which are two different
/// byte paths, and only one of them is certain not to end with a separator.
/// `split_whitespace` absorbed a trailing separator silently; a newline split
/// does not, so the same fix without this filter would hand `from_encoded` an
/// empty string and fail every task with the opposite error.
pub fn split_encoded_list(raw: &str) -> impl Iterator<Item = &str> {
    raw.lines().filter(|s| !s.is_empty())
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

/// The link text of an OUTPUT that is an alias rather than a product, or
/// `None` for anything that should be stored as content.
///
/// A link edge declares `libfoo.so.1.2.3`, `libfoo.so.1` and `libfoo.so` as
/// three outputs, and its command - `cmake -E cmake_symlink_library`, or
/// meson's equivalent - writes one library and two links to it. Storing those
/// by copying THROUGH the link gives three real files of identical size: the
/// soname relationship is lost, each copy is frozen at the bytes it held, and
/// CMake's install-time RPATH rewrite skips a path it believes is a link, so
/// the alias ships with its build-tree RPATH intact. brotli failed nixpkgs'
/// forbidden-reference audit exactly that way.
///
/// ONLY A TARGET CONFINED TO THE LINK'S OWN DIRECTORY. Each output is its own
/// store object, so the text can only be recreated where the sibling exists,
/// which is the build directory. A target carrying a separator could name
/// something outside the build tree that nothing recreates, and is stored as
/// content as before.
pub fn alias_link_target(build_path: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(build_path).is_ok_and(|md| md.file_type().is_symlink()) {
        return None;
    }
    let target = fs::read_link(build_path).ok()?;
    // `parent()` of a bare name is `Some("")`; of `a/b` or `../b` it is not.
    (target.parent() == Some(Path::new(""))).then_some(target)
}

/// WHAT COUNTS AS A HEADER, for every question in this workspace that needs
/// to ask. The driver filters a translation unit's closure with it and the
/// placement below decides a copy with it, and those two answers must be the
/// same answer: when they were written separately they diverged, the copy
/// rule listing four spellings against the driver's eleven, and a compile
/// naming an `.inl` includer failed while the same chain in `.h` built.
///
/// THE LIST IS THE RISK, NOT THE POLARITY. Inverting it is wrong: the filter
/// exists to keep a closure from swallowing generated OBJECTS and SOURCES
/// when a phony is expanded, so "drop what is not provably a header" is the
/// intended direction. What can go wrong is a spelling nobody listed, and
/// the cost is an input dropped and a compile dying on its own include.
///
/// `.hxx` is the one that mattered: it is CMake's OWN spelling for a
/// generated precompiled header (`cmake_pch.hxx`). `.H`, `.tcc` and `.inl`
/// are the other conventions in wide use; `.h++` and `.hp` complete the set.
pub fn header_like(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("h" | "H" | "hh" | "hp" | "hpp" | "hxx" | "h++" | "inc" | "ipp" | "inl" | "tcc")
    )
}

/// Whether an input is a BUILD-TREE EXECUTABLE whose loader search path is
/// `$ORIGIN`-relative, and so must be placed as a COPY rather than a symlink
/// into the store.
///
/// THE SAME SHAPE AS THE `.py` AND `node_modules` ARMS ABOVE, one level down:
/// those exist because a script that realpaths itself lands in the store where
/// its data siblings are not. An ELF with a `$ORIGIN` entry in its RUNPATH has
/// the identical problem, because `$ORIGIN` is the directory of the RESOLVED
/// executable. A symlink into the store therefore makes `$ORIGIN/../orc` mean
/// the STORE's `orc/`, which does not exist, while the sibling library sits in
/// the build tree's `orc/` the whole time.
///
/// MEASURED 2026-10-14 on orc 0.4.44, and proven by difference on the real
/// binary in one directory: `tools/orc`'s `orcc` (RUNPATH
/// `$ORIGIN/../orc`) RUNS as a real file beside `orc/liborc-0.4.so.0`, and
/// dies `error while loading shared libraries: liborc-0.4.so.0` as a symlink
/// into the store. Five task derivations failed this way in the round.
///
/// A REAL-NINJA DIVERGENCE, which is what makes it the driver's: in a real
/// build dir the tool IS a real file, so `$ORIGIN` means the build tree and
/// ninja succeeds. Only this placement breaks it.
///
/// NARROW BY CONSTRUCTION. It asks for a `$ORIGIN` entry specifically, so a
/// tool with an absolute RUNPATH - every compiler wrapper - keeps its symlink
/// and no task gains a copy it did not need. The alternative, copying every
/// ELF input, would copy whole toolchains into every task.
fn origin_relative_executable(source_path: &Path) -> bool {
    // READ THE DYNAMIC SECTION DIRECTLY RATHER THAN SHELLING OUT TO patchelf.
    // The first version called `patchelf --print-rpath`, which needs patchelf
    // on PATH - true inside a task, so it would have worked in production, BUT
    // the predicate answers `false` whenever the command is missing, so a task
    // without it would silently place a tool as a symlink and the orc failure
    // would return with no diagnostic. Its own test caught exactly that: the
    // predicate returned false for `orcc` on a host without patchelf. The
    // `elf` crate is already a dependency of this crate (patchelf.rs uses it),
    // so this reads the same bytes without a subprocess.
    let Ok(data) = fs::read(source_path) else {
        return false;
    };
    let Ok(elf) = ElfBytes::<AnyEndian>::minimal_parse(&data) else {
        return false;
    };
    // Only a dynamic object can carry RUNPATH. `ET_REL` objects cannot, and
    // asking them is wasted work in every compile's own inputs.
    if !matches!(elf.ehdr.e_type, elf::abi::ET_EXEC | elf::abi::ET_DYN) {
        return false;
    }
    let Ok(Some(dyns)) = elf.dynamic() else {
        return false;
    };
    // The dynamic table's strings live in `.dynstr`, reached through the
    // section headers. A FILE WITH NO SECTION HEADERS STILL HAS RUNPATH (the
    // loader reads PT_DYNAMIC), so this answers false there rather than
    // guessing - the polarity the whole predicate uses: an unclassifiable
    // file is placed the way it always was.
    let Ok((shdrs, strtab)) = elf.section_headers_with_strtab() else {
        return false;
    };
    let (Some(shdrs), Some(strtab)) = (shdrs, strtab) else {
        return false;
    };
    let Some(dynstr) = shdrs.iter().find(|sh| {
        strtab
            .get(sh.sh_name as usize)
            .is_ok_and(|n| n == ".dynstr")
    }) else {
        return false;
    };
    let Ok(dynstr_bytes) = elf.section_data(&dynstr) else {
        return false;
    };
    let strs = elf::string_table::StringTable::new(dynstr_bytes.0);
    // `DT_RUNPATH` is the modern entry and `DT_RPATH` the legacy one; glibc
    // reads both, so both are asked.
    let mut entries: Vec<String> = Vec::new();
    for d in dyns.iter() {
        if d.d_tag == elf::abi::DT_RUNPATH || d.d_tag == elf::abi::DT_RPATH {
            if let Ok(s) = strs.get(d.d_val() as usize) {
                entries.push(s.to_string());
            }
        }
    }
    entries.iter().any(|raw| {
        raw.split(':')
            .any(|e| e.starts_with("$ORIGIN") || e.starts_with("${ORIGIN}"))
    })
}

/// What a placement should point AT: the store object's own link text when
/// that object is itself a symlink, and otherwise the store path.
///
/// The alias stored by `alias_link_target` names a sibling, and the sibling
/// exists in the build directory rather than beside the store object.
/// Pointing at the store object would give a link to a link to nothing.
pub fn placement_link_text(source_path: &Path) -> PathBuf {
    if fs::symlink_metadata(source_path).is_ok_and(|md| md.file_type().is_symlink()) {
        if let Ok(text) = fs::read_link(source_path) {
            return text;
        }
    }
    source_path.to_path_buf()
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
    create_symlinks_with(prefix, store_dir, inputs, overwrite, &Default::default())
}

/// `create_symlinks` with the EXTRA copy list a task computes from its own
/// command line. Split rather than given a default argument because Rust has
/// none, and the local-mode caller (`local.rs`) drives a build dir with no
/// command line, so it passes the empty set by using the name above.
pub fn create_symlinks_with(
    prefix: &std::path::Path,
    store_dir: &StoreDir,
    inputs: Vec<DerivedFile>,
    overwrite: bool,
    copy_required: &std::collections::HashSet<std::path::PathBuf>,
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

        // AN ALIAS OUTPUT IS DANGLING IN ITS OWN STORE PATH, AND THAT IS THE
        // CORRECT STATE RATHER THAN A BROKEN INPUT. `exists()` follows the
        // link, so it answers no about exactly the object `alias_link_target`
        // stores: a relative link naming a sibling that lives in a DIFFERENT
        // store object, because each declared output of an edge is its own.
        //     ninja-build-lib-libgtest.so/lib/libgtest.so -> libgtest.so.1.17.0
        // The placement below recreates that TEXT, and the sibling is placed
        // beside it because the driver expands an edge's co-outputs into any
        // consumer's inputs, so the link resolves in the build directory.
        // Asking `symlink_metadata` asks whether the LINK is there, which is
        // the question this guard means.
        //
        // Two gtest tasks died here in a live round on `symlink source does
        // not exist`. brotli, the package the alias fix was written for, is
        // consumed by nothing, so its alias is never placed as an input and
        // the case could not appear there.
        if !source_path.exists() && fs::symlink_metadata(&source_path).is_err() {
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
        // `exists()` FOLLOWS THE LINK, SO A DANGLING PLACEMENT IS INVISIBLE
        // HERE. Before aliases were stored as link text nothing placed could
        // dangle, so this read correctly; now an alias whose sibling is
        // absent is skipped rather than replaced, and the stale link falls
        // through to the duplicate check to be reported as a conflict. The
        // idiom is `local.rs`'s own, six lines above its call: ask both.
        if overwrite && (dest_path.exists() || dest_path.is_symlink()) && !source_path.is_dir() {
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

        // COMPARE AGAINST WHAT A PLACEMENT WOULD HAVE WRITTEN, NOT AGAINST
        // THE STORE PATH. An alias output is placed as its link TEXT, so a
        // second input naming the same alias object finds the destination
        // pointing at `libfoo.so.1.2.3` while `source_path` is the store path
        // - unequal, and the arm below then calls one object a conflict with
        // itself:
        //     /build/.../libgtest_main.so is already a symlink to
        //     libgtest_main.so.1.17.0, and a second input wants it to point
        //     at /nix/store/...-lib-libgtest_main.so/lib/libgtest_main.so
        // The two routes are the co-output expansion handing a consumer both
        // the versioned library and the alias, which is correct; placing the
        // alias twice is idempotent and must read as a duplicate.
        // A genuine collision - two DIFFERENT objects claiming one build path
        // - still compares unequal and still aborts, because their texts
        // differ too.
        if dest_path.is_symlink() {
            match fs::read_link(&dest_path) {
                // ONE ARM, DELIBERATELY. `placement_link_text` returns
                // `source_path` unchanged for any source that is not a
                // symlink, so this already covers every legitimate case. A
                // second arm comparing the raw `source_path` would uniquely
                // accept one thing: a placement left by a driver that pointed
                // AT the alias store object, which is a link to a link to
                // nothing. That is the broken shape, and blessing it would
                // carry it silently across an upgrade.
                Ok(existing) if existing == placement_link_text(&source_path) => continue,
                Ok(existing) => {
                    // THE MESSAGE STATES WHAT IT MEASURED. It used to assert
                    // the claimants were DIFFERENT without opening either,
                    // and in both witnessed instances (dav1d's
                    // `vcs_version.h`, dtc's `version_gen.h`) they held
                    // identical bytes: the store paths differ because one is
                    // a bare upload and the other sits inside a directory
                    // output, which is how each entered the store rather than
                    // what it holds. A reader took the sentence for a
                    // measurement and drew a class distinction from it.
                    // This path is already fatal, so two reads cost nothing,
                    // and the answer points at the class instead of away from
                    // it: identical bytes are one content arriving under two
                    // names, which is the duplicate-upload family.
                    let resolved = dest_path.parent().map(|d| d.join(&existing));
                    let verdict = match (
                        resolved.as_deref().map(fs::read).transpose(),
                        fs::read(&source_path),
                    ) {
                        (Ok(Some(a)), Ok(b)) if a == b => {
                            "The two claimants hold IDENTICAL bytes, so this is one \
                             content entering the store under two names rather than a \
                             conflict."
                        }
                        (Ok(Some(_)), Ok(_)) => "The two claimants hold DIFFERENT bytes.",
                        _ => {
                            "Whether the two claimants agree was not established, \
                             because at least one could not be read."
                        }
                    };
                    return Err(anyhow!(
                        "nix-ninja-task: {} is already a symlink to {}, and a \
                         second input wants it to point at {}. Two files claim \
                         one build path. {}",
                        dest_path.display(),
                        existing.display(),
                        source_path.display(),
                        verdict
                    ));
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
        // AN ALIAS IS EXCLUDED FIRST, AND A HEADER IS WHERE THAT BITES. A
        // store object that is itself a symlink carries link TEXT naming a
        // sibling in another store object, so it is dangling in its own store
        // path BY DESIGN and `fs::copy` on it is ENOENT. Every extension this
        // rule covered before headers was a script, and no build system makes
        // an alias of one; syncqt's forwarding headers are exactly that
        // shape. Asked BEFORE the list rather than appended to it, because
        // `&&` binds tighter than `||`: appended, the exclusion applied to
        // the `node_modules` clause alone, compiled, and left every header
        // copying an alias.
        // A HEADER JOINS FOR THE SAME REASON WITH A DIFFERENT CONSUMER.
        // Under `#pragma GCC system_header`, which CMake's generated
        // precompiled-header wrapper confers on everything it includes, a
        // header reached as a QUOTED SIBLING of an already relocated header
        // resolves its own siblings from the store directory holding that
        // single file. The includer's reported path does not move, which is
        // why an earlier wording here, that a searched header is recorded at
        // its resolved path, described the wrong step. glslang's
        // SymbolTable.h reaches `../Include/Common.h` that way and cannot. A
        // copy keeps the includer in the build tree beside its siblings.
        //
        // `header_like` rather than a list written again here: the two
        // diverged when they were separate, at four spellings against
        // eleven, and `.inl`, `.inc`, `.ipp` and `.tcc` failed this class
        // while `.h` built.
        //
        // A SHADER AND AN ICON JOIN FOR THE SAME REASON WITH TWO MORE
        // CONSUMERS. qsb's glslang includer resolves `#include
        // "../effectlib/x.glsllib"` against the shader's RESOLVED directory,
        // so a linked shader looks for its include in the store and every
        // qtquick3d shader with an include failed `Failed to find include
        // file` (a replay of the kept task: linked exit 1, copied exit 0).
        // The suffixes are the six stages qsb deduces from a file's
        // extension plus the two include spellings. breeze-icons' tools ask
        // `os.path.islink` and `QFileInfo::isSymLink` of each icon and treat
        // a link as an alias, so with every `.svg` placed as a link they
        // processed none.
        //
        // AN EXECUTABLE WITH AN `$ORIGIN` SEARCH PATH IS THE SAME CLASS ONE
        // LEVEL DOWN, and it is asked LAST because it costs a read of the
        // file rather than a test on its name. Those arms exist because a
        // consumer that RESOLVES A PATH lands in the store where its siblings
        // are not; `$ORIGIN` is the loader doing exactly that with the
        // executable's own directory. orc 0.4.44's `orcc` carries
        // `$ORIGIN/../orc` and died on `liborc-0.4.so.0` at exit 127 across
        // five task derivations while the library and a correct alias chain
        // sat in the build tree's `orc/`. See `origin_relative_executable`
        // for the proof and for why the predicate is narrow.
        let copy_not_link = !source_path.is_symlink()
            && (input
                .build_path
                .extension()
                .is_some_and(|e| e == "py" || e == "mjs" || e == "js" || e == "cjs")
                || header_like(&input.build_path)
                || matches!(
                    input.build_path.extension().and_then(|e| e.to_str()),
                    Some(
                        "vert"
                            | "tesc"
                            | "tese"
                            | "geom"
                            | "frag"
                            | "comp"
                            | "glsl"
                            | "glsllib"
                            | "svg"
                            | "svgz"
                    )
                )
                || input
                    .build_path
                    .components()
                    .any(|c| c.as_os_str() == "node_modules")
                || copy_required.contains(&input.build_path)
                || origin_relative_executable(&source_path));
        if copy_not_link {
            // THE DUPLICATE CHECK ABOVE ASKS ONLY ABOUT LINKS, and a copied
            // header is not one. The driver keys its input set by the build
            // path as SPELLED, so one file discovered by two routes arrives
            // twice, once relative and once as a `../` climb resolving back
            // to the same destination; qtsvg measured that shape. While
            // every placement was a symlink the second arrival compared link
            // texts and was skipped. A regular file reaches neither arm, so
            // it fell through to a copy onto an existing destination, and a
            // store file is 0444, which makes that EACCES rather than a
            // duplicate. Same bytes is the same file arriving twice.
            if dest_path.is_file() {
                match (fs::read(&dest_path), fs::read(&source_path)) {
                    (Ok(a), Ok(b)) if a == b => continue,
                    _ => {
                        return Err(anyhow!(
                            "nix-ninja-task: {} is already a file, and a second input wants \
                             it to hold {:?}, whose bytes differ or could not be read",
                            dest_path.display(),
                            source_path
                        ))
                    }
                }
            }
            fs::copy(&source_path, &dest_path)
                .map_err(|e| anyhow!("copy({:?} -> {}): {e}", source_path, dest_path.display()))?;
            // `fs::copy` CARRIES THE SOURCE MODE, and every store file is
            // 0444. A later task that has to overwrite this one then dies
            // EACCES, which is the same defect `copy_tree` already repairs
            // a few lines below for the same reason.
            let mut perms = fs::metadata(&dest_path)
                .map_err(|e| anyhow!("metadata({}): {e}", dest_path.display()))?
                .permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(&dest_path, perms)
                .map_err(|e| anyhow!("set_permissions({}): {e}", dest_path.display()))?;
            continue;
        }

        // A STORE OBJECT THAT IS ITSELF A SYMLINK CARRIES A NAME, NOT A
        // PLACE. `copy_outputs_to_placeholders` stores an alias output as a
        // link whose target is a bare sibling filename, because each output
        // is its own store object and a resolved path would point at a
        // directory the sibling is not in. Recreating the TEXT here is what
        // makes it resolve: in the build directory the sibling is the
        // library it names. Pointing at the store object instead would give
        // a link to a link to nothing.
        let link_to = placement_link_text(&source_path);
        symlink(&link_to, &dest_path).map_err(|e| {
            anyhow!(
                "Failed to create symlink from {:?} to {}: {}",
                link_to,
                dest_path.display(),
                e
            )
        })?;
    }

    Ok(())
}

/// Store one output at its placeholder path. The alias, directory and
/// plain-file cases are decided here so a test outside this crate's
/// fileset can run the same step the task binary runs.
pub fn store_output(build_path: &Path, target_path: &Path) -> Result<()> {
    // A DIRECTORY OUTPUT IS THE ONLY WAY TO CARRY WHAT A RULE DOES NOT
    // DECLARE. syncqt writes hundreds of forwarding headers whose names
    // ninja never knows, so the driver declares the TREE and this copies
    // whatever is in it. fs::copy is file-to-file and would fail with
    // EISDIR here, which reads as a permissions problem.
    // AN OUTPUT THAT IS A SYMLINK IS A NAME, AND `fs::copy` FOLLOWS IT.
    // A link edge declares `libfoo.so.1.2.3`, `libfoo.so.1` and
    // `libfoo.so` as three outputs, and the command - `cmake -E
    // cmake_symlink_library` - writes one library and two links to it.
    // Copying through them stored three real files of identical size,
    // which loses the soname relationship and, because CMake's
    // install-time RPATH rewrite skips a path it believes is a link,
    // shipped the alias with its build-tree RPATH intact. brotli failed
    // nixpkgs' forbidden-reference audit that way.
    //
    // The LINK TEXT is what is stored, not a resolved path. Each output
    // is its own store object, so a target naming a sibling resolves
    // against the build directory and nowhere else - which is where
    // `create_symlinks` recreates it. Same reasoning as the
    // NIX_NINJA_ALIASES mechanism, which carries text for the same
    // reason and cannot cover this case: it is fed by the configure-time
    // build-directory scan, and these links are made by an edge.
    //
    // Only a target confined to the link's own directory. Anything with
    // a separator could climb out of the build tree, where nothing
    // recreates it.
    if let Some(target) = alias_link_target(build_path) {
        if target_path.exists() || target_path.is_symlink() {
            fs::remove_file(target_path).ok();
        }
        symlink(&target, target_path).map_err(|e| {
            anyhow::anyhow!(
                "symlink({} -> {}) for alias output: {e}",
                target_path.display(),
                target.display()
            )
        })?;
        return Ok(());
    }
    if build_path.is_dir() {
        copy_tree(build_path, target_path)?;
        return Ok(());
    }
    fs::copy(build_path, target_path).map_err(|e| {
        anyhow::anyhow!(
            "copy({} -> {}): {e}",
            build_path.display(),
            target_path.display()
        )
    })?;
    Ok(())
}

/// Copy a directory recursively, following nothing. Symlinks inside a build
/// tree point at materialized inputs, so copying the LINK would carry a path
/// that does not exist on the far side; copying what it resolves to is what
/// the consumer needs.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .map_err(|e| anyhow::anyhow!("create_dir_all({}): {e}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|e| anyhow::anyhow!("read_dir({}): {e}", src.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // metadata() follows the link; file_type() would not.
        // A TOOL'S OWN INCREMENTAL-STATE DIRECTORY MUST NOT BE CAPTURED.
        // syncqt writes `.syncqt_staging` beside the headers it generates
        // and reads it to decide the sync is already done. Copying it into
        // the tree output makes it an INPUT on the next run, syncqt then
        // skips, and the task succeeds while generating nothing - the output
        // is byte-identical every time, which reads as determinism rather
        // than as a short circuit. Measured on qtsvg: eight files captured,
        // three of them staging, and no private forwarding headers ever.
        // Dot-directories generally, because this is what tools use for the
        // purpose and none of them belongs in a declared output.
        // EXCEPT `.syncqt_staging` ITSELF, since 2026-08-23: Qt's install
        // rule installs the module's public headers FROM that directory
        // (`file(INSTALL ... include/QtSvg/.syncqt_staging)`), so a tree
        // without it installs a library with no headers. The short circuit
        // the exclusion guarded against is closed at its cause instead: the
        // syncqt branch in main() deletes any staging dir that arrived as an
        // input before the tool runs, so syncqt never finds a sync "done".
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && name != ".syncqt_staging" {
            continue;
        }
        let md = match fs::metadata(&from) {
            Ok(m) => m,
            // A DANGLING LINK IS NOT AN ERROR HERE. A build tree carries
            // links to inputs that were materialized for a different task;
            // failing on one would turn a complete output into no output.
            Err(_) => continue,
        };
        if md.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| {
                anyhow::anyhow!("copy({} -> {}): {e}", from.display(), to.display())
            })?;
            // fs::copy carries the source mode, and a generated header is
            // often 0444. A later task that has to overwrite it dies EACCES,
            // which is the same defect the duplicate-output fix already met.
            let mut perms = fs::metadata(&to)
                .map_err(|e| anyhow::anyhow!("metadata({}): {e}", to.display()))?
                .permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = fs::set_permissions(&to, perms);
        }
    }
    Ok(())
}

#[cfg(test)]
mod link_tree_tests {
    use super::link_tree;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// UPSTREAM #5's second half is this function.
    ///
    /// The issue asks that, in local mode, a phony target's contents be
    /// walked and each file symlinked back into the build directory "to have
    /// the same side-effect and make it available to the user". Resolution -
    /// following phony aliases to concrete outputs - has five tests in
    /// task.rs. The materialisation had none, so the half that actually
    /// touches the user's build directory was the untested one.
    ///
    /// No `tempfile` here on purpose: `nix-ninja-task` has no
    /// dev-dependencies, and adding one writes a line into `Cargo.lock`,
    /// which is inside this crate's own fileset allowlist. A future bump of
    /// a test-only dependency would then re-key every banked per-TU output.
    /// See the DEFER on divan in `crates/nix-ninja/Cargo.toml`.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "nn-link-tree-{}-{tag}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_tree_is_linked_file_by_file_and_its_directories_stay_real() {
        let tmp = Tmp::new("shape");
        let src = tmp.path().join("store-out");
        fs::create_dir_all(src.join("include/qt")).unwrap();
        fs::write(src.join("include/qt/QObject"), b"header").unwrap();
        fs::write(src.join("top.h"), b"top").unwrap();

        let dst = tmp.path().join("build");
        link_tree(&src, &dst).unwrap();

        // Files arrive as symlinks pointing INTO the source tree.
        assert!(dst.join("top.h").is_symlink());
        assert_eq!(fs::read_link(dst.join("top.h")).unwrap(), src.join("top.h"));
        assert_eq!(fs::read(dst.join("include/qt/QObject")).unwrap(), b"header");

        // Directories are REAL directories, not symlinks. This is the whole
        // reason the tree is walked rather than linked whole: a symlinked
        // directory is read-only store, so a later declared output has
        // nowhere to land and any task writing into it dies EACCES.
        assert!(dst.join("include").is_dir());
        assert!(!dst.join("include").is_symlink());
        assert!(!dst.join("include/qt").is_symlink());
    }

    #[test]
    fn an_existing_file_is_left_alone_because_the_declared_output_wins() {
        let tmp = Tmp::new("firstwins");
        let src = tmp.path().join("store-out");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("shared.h"), b"from the tree").unwrap();

        let dst = tmp.path().join("build");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join("shared.h"), b"declared output").unwrap();

        link_tree(&src, &dst).unwrap();

        // create_symlinks sorts directory inputs LAST so the individually
        // declared file is already in place; the tree must then fill only
        // what is missing rather than overwrite it.
        assert_eq!(fs::read(dst.join("shared.h")).unwrap(), b"declared output");
        assert!(!dst.join("shared.h").is_symlink());
    }

    /// The skip guard has two halves, `exists()` and `is_symlink()`, and
    /// the arm above pre-places a REGULAR file, which the first half alone
    /// rejects. An alias output is placed as a DANGLING link, for which
    /// `exists()` answers no; only `is_symlink()` keeps link_tree from
    /// failing with EEXIST on top of it. Reverting that half fails here.
    #[test]
    fn a_dangling_link_already_at_the_destination_is_left_alone() {
        let t = Tmp::new("dangling");
        let src = t.path().join("src");
        let dst = t.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("libfoo.so"), b"from the tree").unwrap();
        fs::create_dir_all(&dst).unwrap();
        std::os::unix::fs::symlink("libfoo.so.1.2.3", dst.join("libfoo.so")).unwrap();
        assert!(
            !dst.join("libfoo.so").exists(),
            "the fixture must be DANGLING"
        );

        link_tree(&src, &dst).unwrap();

        assert_eq!(
            fs::read_link(dst.join("libfoo.so")).unwrap(),
            Path::new("libfoo.so.1.2.3"),
            "the placed alias must survive the tree walk"
        );
    }

    #[test]
    fn the_destination_is_created_when_it_does_not_exist() {
        let tmp = Tmp::new("mkdir");
        let src = tmp.path().join("store-out");
        fs::create_dir_all(src.join("a/b")).unwrap();
        fs::write(src.join("a/b/c.txt"), b"deep").unwrap();

        let dst = tmp.path().join("does/not/exist/yet");
        link_tree(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("a/b/c.txt")).unwrap(), b"deep");
    }
}

/// Whether a RELATIVE directory spelling may be created against `cwd`.
///
/// A compile resolves `-I../subprojects/gvdb` literally, so the directory has
/// to exist at that exact climbing path and cannot be remapped the way
/// `.nn-up` remaps a climbing OUTPUT. The categorical refusal of `..` that
/// stood here before was the reason glib could not build: the driver names
/// both spellings and only the build-tree one was ever carried.
///
/// LEXICAL ON PURPOSE, no `canonicalize`. Resolving through symlinks would
/// let a link placed between the check and the `create_dir_all` move the
/// target out of the tree, and the check has to describe the path that is
/// actually created.
///
/// The bound is the sandbox root, which is `cwd`'s first component: a build
/// directory is somewhere under it, so a path that pops past it is leaving
/// the tree rather than addressing a sibling of the build directory. This is
/// the same discriminator the emitted-output side settled on, where climbing
/// alone is not the test because openfec legitimately climbs.
pub fn confined_relative_dir(cwd: &Path, dir: &Path) -> bool {
    use std::path::Component;
    // An absolute spelling is refused by the catch-all in the loop, whose
    // first component is `RootDir`. An `is_absolute` test here as well read
    // as defence in depth and was none: no input reaches one guard without
    // reaching the other, so a mutant deleting it survived every arm.
    let mut parts: Vec<std::ffi::OsString> = cwd
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_owned()),
            _ => None,
        })
        .collect();
    // The first component is the floor, not a step: popping it leaves the
    // tree even though the resolved path is still non-empty.
    const FLOOR: usize = 1;
    if parts.len() < FLOOR {
        return false;
    }
    for c in dir.components() {
        match c {
            Component::Normal(n) => parts.push(n.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.len() <= FLOOR {
                    return false;
                }
                parts.pop();
            }
            _ => return false,
        }
    }
    parts.len() > FLOOR
}

#[cfg(test)]
mod confined_relative_dir_tests {
    use super::confined_relative_dir;
    use std::path::Path;

    #[test]
    fn a_sibling_of_the_build_directory_is_allowed_and_an_escape_is_not() {
        let cwd = Path::new("/build/source/build");
        // glib's own spelling, the case that was refused outright.
        assert!(confined_relative_dir(cwd, Path::new("../subprojects/gvdb")));
        // Controls, without which a function returning true would pass the
        // arm above: the build-tree spelling still holds, and each way out of
        // the tree is refused.
        assert!(confined_relative_dir(cwd, Path::new("subprojects/gvdb")));
        assert!(!confined_relative_dir(cwd, Path::new("../../../etc")));
        assert!(!confined_relative_dir(cwd, Path::new("/etc")));
        assert!(!confined_relative_dir(cwd, Path::new("../..")));
        // Popping to exactly the floor is out, since the floor is the
        // sandbox root and not a directory a build may write into.
        assert!(!confined_relative_dir(cwd, Path::new("../../")));
        // A climb that comes back down inside the tree is fine.
        assert!(confined_relative_dir(cwd, Path::new("../build/gen")));
        // THE FLOOR IS LOAD BEARING AND THE OTHER ESCAPE CASES DO NOT SHOW
        // IT. Each of those pops to nothing and then pushes one component,
        // so the closing length test refuses them whether or not the floor
        // is checked, and a mutant deleting the floor survived them all.
        // This one climbs PAST the root and descends far enough to end up
        // longer than the floor, so only the in-loop check refuses it.
        assert!(!confined_relative_dir(
            cwd,
            Path::new("../../../../nix/store/evil")
        ));
        assert!(!confined_relative_dir(cwd, Path::new("../../../../a/b")));
    }
}

#[cfg(test)]
mod encoded_list_tests {
    use super::{split_encoded_list, ENCODED_LIST_SEP};

    // THE BOUNDARY IS THE SUBJECT, NOT THE TRIPLE. A test that encodes one
    // path with a space and decodes it passes under the old separator too,
    // because nothing crosses an entry boundary. libepoxy's failure needs TWO
    // entries with the space in the first, which is the only shape where a
    // space-joined list hands the second entry's parser the first entry's
    // tail. A mutant restoring `" "` fails this and passes a single-entry
    // version of it.
    #[test]
    fn a_build_path_with_a_space_does_not_split_the_list() {
        let entries = [
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-o:test/khronos typedefs.p/k.c.o:",
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-p:plain/other.c.o:",
        ];
        let joined = entries.join(ENCODED_LIST_SEP);
        let got: Vec<&str> = split_encoded_list(&joined).collect();
        assert_eq!(got, entries, "a space inside an entry split the list");
    }

    // passAsFile and an inline environment variable are two different byte
    // paths and only one is certain not to end with a separator. Without the
    // empty filter this yields a third, empty entry, which `from_encoded`
    // reports as a non-absolute store path: the same failure the fix removes,
    // arriving from the opposite direction.
    #[test]
    fn a_trailing_separator_is_not_an_entry() {
        let raw = format!("a:b:{sep}c:d:{sep}", sep = ENCODED_LIST_SEP);
        assert_eq!(split_encoded_list(&raw).count(), 2);
        assert_eq!(split_encoded_list("").count(), 0);
    }

    /// THE PREDICATE THAT DECIDES A PLACEMENT, against REAL store ELFs rather
    /// than fixtures: this crate has no dev-dependencies on purpose (see the
    /// note in `link_tree_tests`), and a hand-built ELF would be a fixture
    /// whose bytes drift from what the loader actually reads.
    ///
    /// orc 0.4.44's `orcc` carries `$ORIGIN/../orc` in its RUNPATH, which is
    /// the shape that must be COPIED. A compiler wrapper carries absolute
    /// store paths and must stay a SYMLINK, and it is the negative control: a
    /// predicate that simply answered true would copy whole toolchains into
    /// every task.
    ///
    /// THE FIXTURE IS DERIVED FROM WHAT THIS CRATE ALWAYS HAS, not from a
    /// store path a build sandbox may not see. The first version named orcc's
    /// path and ASSERTED it present, which broke this crate's OWN derivation:
    /// `nix build .#nix-ninja-task` runs these tests in a sandbox where that
    /// path is not an input, so the fix could not build at all. The version
    /// before that used `if path.exists()` and would have passed while proving
    /// nothing once collected. Both traps are recorded in this tree, and the
    /// answer to them is a fixture the test can always reach.
    #[test]
    fn an_origin_relative_tool_is_recognised_and_an_absolute_one_is_not() {
        use super::origin_relative_executable;
        use std::path::Path;

        // THE TASK BINARY ITSELF IS THE NEGATIVE CONTROL: it carries an
        // absolute RUNPATH, and it is present wherever this test runs.
        let self_path = std::env::current_exe().expect("current_exe");
        assert!(
            !origin_relative_executable(&self_path),
            "the task binary has an absolute RUNPATH and must keep its symlink"
        );

        // A NON-ELF AND A MISSING FILE both answer false, so the predicate is
        // not simply true for every path the test can reach.
        assert!(!origin_relative_executable(Path::new("/etc/hostname")));
        assert!(!origin_relative_executable(Path::new("/nonexistent/nope")));

        // THE SUBJECT: orcc carries `$ORIGIN/../orc`, which is the shape that
        // must be COPIED. It is exercised when the store holds it and the arm
        // SAYS SO when it does not, because a fixture that cannot run must be
        // loud rather than silently green - but it must not FAIL the build for
        // a path the sandbox cannot see. The end-to-end check is the gate, not
        // this test.
        let orcc = Path::new(
            "/nix/store/4m052g3ncdx7iq0krjpk6rqq6lyswqbr-ninja-build-tools-orcc/tools/orcc",
        );
        if orcc.exists() {
            assert!(
                origin_relative_executable(orcc),
                "orcc carries $ORIGIN/../orc and must be placed as a copy"
            );
        } else {
            eprintln!(
                "NOTE: orcc is not in the store, so the $ORIGIN subject arm was \
                 not exercised here; the negative control above still ran"
            );
        }
    }
}
