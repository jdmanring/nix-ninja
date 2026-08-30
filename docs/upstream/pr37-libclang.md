# PR #37, libclang for dependency inference: read, and the answer is BOTH

Draft, open since 2025-08-28, no comments, untouched since the day it was
opened. 181 lines in `crates/deps-infer/src/clang_infer.rs`. Read in full on
2026-08-30.

**The senior answer is not "ours is better". It is that these are two stages of
one pipeline, this fork already has that pipeline, and #37 is a better engine
for the stage we currently fill with `gcc -M`.**

## What #37 gets right, and it is the thing our scanner cannot do

It runs a real preprocessor. `clang_parseTranslationUnit` plus
`clang_getInclusions` returns what the compiler ACTUALLY includes: `#ifdef`
branches resolved against the real `-D` set, computed includes expanded,
macro-built paths correct. Our scanner is textual and cannot do any of that -
`CONTRIBUTING.md` says so in the maintainer's own words, and this fork has
three separate patches (same-file macro defines, cross-file defines, a
function-like-macro detector) that are all approximations of one preprocessor.

If the question were "which produces the more accurate include set for a
well-formed C++ TU", #37 wins and it is not close.

## Why it cannot be the DEFAULT stage, with numbers

**Cost.** Measured on this fork driving `example-nix`, 345 tasks:
`scan 1174/86792 parsed` - 98.6% of scan requests hit the memo, and a parse
costs ~30 us per TU. A libclang parse is milliseconds and each TU is a fresh
`clang_parseTranslationUnit`; #37's own comment concedes the point - "the real
win would be to reuse parsed headers, but that's complex with current API". The
driver's resolve phase is 897 ms for 345 tasks today. On the qtwebengine graph
this fork drives, the task count is at least 16,077. Replacing a 30 us memoized
scan with a millisecond parse makes inference the bottleneck on exactly the
graphs the tool exists for.

**Silent under-declaration, and this is the blocking one.** #37 handles a parse
failure like this:

    Err(_) => {
        // Skip files that can't be parsed (e.g., missing generated headers)
        continue;
    }

The comment names the case and the code drops it. A TU whose generated header
does not exist yet contributes ZERO includes, the task derivation is built
without them, and the compile fails inside the sandbox with a missing header -
or worse, succeeds against a different one. That is the failure polarity this
whole crate is built against: over-declaring costs one upload, under-declaring
ships the wrong artifact. Our scanner's equivalent path was fixed on
2026-08-30 to declare the header and skip only the READ, with a negative
control asserting that a file nobody declared generated still fails loudly.

**It cannot see non-C inputs at all.** nasm and yasm sources use
`%include "x86inc.asm"`. libclang cannot parse a `.asm` file, so
`parse_file_includes` errors, the `Err(_)` arm skips it, and libvmaf's
`cpuid.asm` regresses to a silent zero-include answer. Our tenth input class
(2026-08-23) exists because that exact file failed loudly.

**Closure cost.** `clang_sys` links libclang at runtime. The driver runs inside
every dynamic task derivation, so libclang enters the closure of every build -
including builds that use gcc and never touch clang.

**System-header filtering is wrong under nix.** #37 drops anything
`clang_Location_isInSystemHeader` reports. Under nix there is no system: every
header is a store path reached through `-I` or `-isystem`, and a dependency
dropped for looking systemy is a missing input.

## The design this fork already runs, which is where #37 belongs

`discover_c_includes` is already two-stage, and it was built that way for the
reason #37 exists:

1. the textual scan runs, cheap and memoized, and **reports whether it could
   answer** (`retrieve_c_includes_checked` returns an `incomplete` flag - a
   function-like computed include sets it);
2. only for TUs the scan could not answer, a real preprocessor runs
   (`gcc_depfile.rs`, `gcc -M`), and the two answers are UNIONED, scan order
   first.

The union is deliberate: neither is a superset of the other. The scan is blind
to computed includes; the depfile is blind to pragma-marked build-dir headers,
which is every autotools package carrying gnulib. Measured on gnum4-1.4.21.

**So the seam #37 needs already exists.** libclang is a candidate backend for
stage 2, replacing the `gcc -M` subprocess. That is a contained change, keeps
the 98.6% memo hit rate on the cheap path, and pays libclang's cost only on the
TUs that actually need a preprocessor.

## One argument for `gcc -M` over libclang even at stage 2

The build's own compiler is the authority on what the build will include. If
the package compiles with gcc, `gcc -M` answers with gcc's ifdef and macro
semantics; libclang answers with clang's, and where they disagree the
derivation's inputs disagree with the compile that will run. `gcc -M` also
needs no new dependency, since the compiler is already in the task's closure by
construction.

The counter is real and worth stating: `gcc -M` costs a process spawn per TU
and libclang does not. On the current fallback rate that is a small number of
spawns, so the trade favours fidelity. If the fallback rate rose, it would not.

## What to say in the PR, and what NOT to say

Do not open with the defects. It is his draft, a year old, with no comment on
it, and the first thing he should read is that somebody ran it against a real
graph and thought about where it fits.

The offer is: the two-stage pipeline with the `incomplete` signal, the measured
memo hit rate that says why stage 1 must stay cheap, and the four input classes
(nasm, non-UTF-8 sources, a directory shadowing a header, generated headers)
that a preprocessor of ANY kind cannot resolve because they are questions about
which files exist. Then the `Err(_)` arm, as a note rather than a verdict -
under-declaration is silent and it is the one thing that must not ship either
way.
