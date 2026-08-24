# The rustc drop-in: per-crate resumability for cargo, by design note

Status: DESIGN, not built. Written 2026-08-24, when the last three
unresumable derivations in an ArtNix edition turned out to be cargo-c
builds (libdovi, rav1e, libimagequant) that no existing route covers:
crate2nix cannot emit cargo-c's C-ABI artifacts (the manifests declare
no staticlib/cdylib crate-type; cargo-c synthesizes it at build time),
and the cc drop-in never sees a rustc invocation.

## The shape

Same pattern as the cc drop-in, one layer up: cargo already runs ONE
rustc invocation per crate, so a `rustc` shim on PATH that turns each
invocation into a one-edge ninja file gives per-crate resumability to
every cargo consumer - plain cargo, cargo-c, cargo cbuild - without
converting anything, and would retire crate2nix conversions here.

1. Passthrough: `--print`, `-vV`, `--emit=dep-info` alone, anything
   outside the build top.
2. Dep discovery: run the real rustc with `--emit=dep-info` only
   (parses, no codegen, fast) into the depfile the ninja edge names;
   the driver's depfile_read_back path replaces the C scanner, which
   cannot parse Rust.
3. Outputs: `--print file-names` with the same argv names every file
   the invocation writes (rlib, rmeta, cdylib, proc-macro dylib);
   declare all of them on the edge so they land back in the build dir.
4. Inputs beyond the depfile: `--extern name=path` arguments name
   build-dir rlibs from other tasks - the driver needs an --extern
   parser the way it has a gcc include parser, routing graph-known
   paths as Built inputs. `-L dependency=<dir>` similarly.
5. Environment: CARGO_*, OUT_DIR (build-script output consumed at
   compile time - the OUT_DIR tree is an input dir to upload), and
   proc-macro dylibs, which later rustc invocations LOAD, so they must
   be inputs of every dependent task.

## Why it is not a night's work

The cc drop-in accumulated ten-plus defect classes across days, each
found by a real build; this shim has more moving parts (multi-output
edges, extern graphs, OUT_DIR trees, proc-macro loading). Building it
against shipped media libraries at the end of a campaign night is how
plausible-but-wrong ships. The interim disposition for the three
packages is the gate's designed one: James authorizes their one-time
~10-minute build, the cache holds the outputs, and the shim retires
the class when it lands.
