//! LOCAL EDGES: the few edges the outer driver runs itself, in the package
//! sandbox, instead of emitting a task derivation for them.
//!
//! qtwebengine's CMake layer drives a second build system. A `cmake -P
//! QtGnGen.cmake` edge runs gn, which writes a whole build.ninja tree under
//! `src/core/Release/x86_64` that no edge declares, and a later edge runs
//! `ninja -C` over that tree. As task derivations both are wrong by
//! construction: the generator's tree stays in its task's output, and the
//! nested ninja runs in a sandbox with no daemon and none of that tree. Run
//! here, the generator writes into the build dir and the nested ninja is the
//! consumer's own drop-in, so every translation unit under it still becomes
//! its own derivation.
//!
//! The class is closed and named, never a shape: a member is added with the
//! edge that needs it.

use crate::task::{command_argv, is_shell_assignment};

/// Disables both members, for a gate's negative control.
pub(crate) const DISABLE_VAR: &str = "NIX_NINJA_NO_LOCAL_EDGES";

/// The width a nested driver runs at: the job permits the outer runner holds
/// while the edge runs. Read ahead of `-j`, since qtwebengine bakes the
/// configure machine's `-j3` into the command.
pub(crate) const JOBS_VAR: &str = "NIX_NINJA_LOCAL_EDGE_JOBS";

/// The gn generator scripts a `cmake -P` member runs.
const GN_SCRIPTS: [&str; 2] = ["/cmake/QtGnGen.cmake", "/cmake/QtGnCredits.cmake"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Member {
    /// `ninja -C <dir>` where `ninja` is the consumer's drop-in.
    NestedNinja,
    /// `cmake -P <...>/cmake/QtGnGen.cmake` and its credits sibling.
    GnScript,
}

impl std::fmt::Display for Member {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Member::NestedNinja => "nested ninja",
            Member::GnScript => "gn generator",
        })
    }
}

/// Whether `cmdline` is a local edge, and which member.
///
/// `drop_in_set` is whether `NN_NIX_NINJA` is set, the consumer's mark that
/// `ninja` on PATH is its drop-in; `is_drop_in` answers whether a program
/// word names the same file as that `ninja`. Both are parameters so the
/// predicate reads nothing from the process.
pub(crate) fn local_edge(
    cmdline: &str,
    drop_in_set: bool,
    is_drop_in: impl Fn(&str) -> bool,
) -> Option<Member> {
    let mut argv = command_argv(cmdline)?;
    let mut program = unquote(argv.next()?);
    let mut args: Vec<&str> = argv.collect();
    if basename(program) == "cmake" {
        if args.get(..2) == Some(&["-E", "env"]) {
            // `cmake -E env [NAME=VALUE | --unset=NAME | --modify ...]... [--]
            // command`: the program is the first word none of those.
            let mut rest = args[2..].iter();
            program = loop {
                match *rest.next()? {
                    "--" => break unquote(rest.next()?),
                    "--modify" => {
                        rest.next();
                    }
                    w if w.starts_with("--unset=") || is_shell_assignment(w) => {}
                    w => break unquote(w),
                }
            };
            args = rest.copied().collect();
        } else {
            let script = args
                .iter()
                .position(|a| *a == "-P")
                .and_then(|i| args.get(i + 1))?;
            return GN_SCRIPTS
                .iter()
                .any(|s| unquote(script).ends_with(s))
                .then_some(Member::GnScript);
        }
    }
    let names_dir = args.iter().enumerate().any(|(i, a)| {
        (*a == "-C" && args.get(i + 1).is_some()) || a.len() > 2 && a.starts_with("-C")
    });
    (drop_in_set && names_dir && is_drop_in(program)).then_some(Member::NestedNinja)
}

fn unquote(w: &str) -> &str {
    w.trim_matches(|c| c == '"' || c == '\'')
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::{local_edge, Member};

    const DROP_IN: &str = "/nix/store/xnr8x7dgl8za86ib6w1n4qp8i984y50f-nix-ninja-hook/bin/ninja";

    /// qtwebengine build.ninja:750, verbatim but for the store hashes.
    const NESTED: &str = "cd /build/qtwebengine-everywhere-src-6.11.2/build/src/core && \
        /nix/store/7yy1d055r9sdx22r02bqbpb0x66ms3m4-cmake-4.4.3/bin/cmake -E env \
        NODEJS_EXECUTABLE=/nix/store/gxq2cd70i077rah1d4hkzc1lpq8q4pv8-nodejs-24.21.0/bin/node \
        /nix/store/xnr8x7dgl8za86ib6w1n4qp8i984y50f-nix-ninja-hook/bin/ninja -j3 \
        -C /build/qtwebengine-everywhere-src-6.11.2/build/src/core/Release/x86_64 QtWebEngineCore";

    /// qtwebengine build.ninja:735, the gn run.
    const GN_GEN: &str = "cd /build/qtwebengine-everywhere-src-6.11.2/build && \
        /nix/store/7yy1d055r9sdx22r02bqbpb0x66ms3m4-cmake-4.4.3/bin/cmake \
        -DBUILD_DIR=/build/qtwebengine-everywhere-src-6.11.2/build/src/core/Release/x86_64 \
        -DSOURCE_DIR=/build/qtwebengine-everywhere-src-6.11.2/src/core -DMODULE=core \
        -DQT_HOST_PATH= -DGN_THREADS= -DQT_ALLOW_SYMLINK_IN_PATHS=OFF \
        -P /build/qtwebengine-everywhere-src-6.11.2/cmake/QtGnGen.cmake";

    fn is_drop_in(p: &str) -> bool {
        p == DROP_IN
    }

    #[test]
    fn the_nested_ninja_under_cmake_env_is_a_member() {
        assert_eq!(
            local_edge(NESTED, true, is_drop_in),
            Some(Member::NestedNinja)
        );
        // Bare, with the `-C` joined.
        assert_eq!(
            local_edge(&format!("{DROP_IN} -Csub all"), true, is_drop_in),
            Some(Member::NestedNinja)
        );
    }

    #[test]
    fn both_gn_scripts_are_members() {
        assert_eq!(
            local_edge(GN_GEN, false, is_drop_in),
            Some(Member::GnScript)
        );
        let credits = GN_GEN.replace("QtGnGen.cmake", "QtGnCredits.cmake");
        assert_eq!(
            local_edge(&credits, false, is_drop_in),
            Some(Member::GnScript)
        );
    }

    /// A real ninja is not the consumer's drop-in, whatever its argv.
    #[test]
    fn a_ninja_that_is_not_the_drop_in_is_not() {
        let other = NESTED.replace(DROP_IN, "/nix/store/aaaa-ninja-1.13.2/bin/ninja");
        assert_eq!(local_edge(&other, true, is_drop_in), None);
    }

    #[test]
    fn a_cmake_script_of_another_name_is_not() {
        let other = GN_GEN.replace("QtGnGen.cmake", "QtGnHelpers.cmake");
        assert_eq!(local_edge(&other, true, is_drop_in), None);
        // The script's NAME decides, not a substring elsewhere on the line.
        let mentioned = format!("{} -DX=/s/cmake/QtGnGen.cmake", other);
        assert_eq!(local_edge(&mentioned, true, is_drop_in), None);
    }

    #[test]
    fn without_nn_nix_ninja_the_drop_in_is_not() {
        assert_eq!(local_edge(NESTED, false, is_drop_in), None);
    }

    /// The pair the guard reads: drop-in AND `-C`. Without `-C` the drop-in
    /// builds the outer tree itself.
    #[test]
    fn the_drop_in_without_a_directory_is_not() {
        let no_dir = NESTED.replace(" -C ", " ");
        assert_eq!(local_edge(&no_dir, true, is_drop_in), None);
    }
}
