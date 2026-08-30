# DRAFT: the bug-fix batch

Not sent. Five fixes landed on this fork between 2026-08-27 and 2026-08-29,
all in nix-ninja's own crates, none of them a heuristic and none of them
depending on the dependency-inference argument that blocks PR 1 in
`pr-plan.md`. That file already says this class must travel separately:
error-and-bug fixes "are BUG fixes and should not be bundled with heuristics
at all". This is that PR.

Read against the forge on 2026-08-29. None of the five has an open issue.
`f8bb3bd` sits in the mechanism of their CLOSED #1 ("Outputs needs to be
patchelf'd to canonical /nix/store paths"), which is where a reviewer will
want the context, and reopening somebody's closed issue to say so is worse
than saying it in the PR body.

## What is in it, in the order a reviewer should read it

1. **`bash` was never an input of a task derivation** (`2f7b8f2`, the
   `drv.inputs.insert(tools.bash)` half). Every task's command runs through a
   shell, and the shell was reaching the sandbox by whatever else happened to
   drag it in. This is one line and it is the one a maintainer will care most
   about, because it says the input set was incomplete for every task.
2. **A directory on the include path is not a header** (`4e591a0`).
   `canonicalize_cached` gated resolution on `path.exists()`, which is true
   for a directory, so `#include <memory>` next to a `memory/` directory
   resolved to the directory, the BFS queued it, and `fs::read` failed
   `EISDIR`. gcc skips a directory and keeps searching; this now does too.
   Carries a regression test that was verified failing first.
3. **`cc` resolves lazily** (`7fb756e`). `Tools::new()` resolved every tool
   eagerly and fallibly, so a build with zero compile edges died on `cc:
   command not found` before doing any work, having demanded a tool it was
   never going to use. Any target-less graph hits this; hicolor-icon-theme is
   the package that bit us.
4. **Generated headers materialize through a virtual-path map** (`2f7b8f2`,
   `dc296f3`). The include scanner reads headers off disk, and a header a
   generator has not written yet is not there, so the compile task never
   declared it. Known-generated inputs now map to themselves so resolution
   succeeds before the file exists; task outputs are excluded from the map,
   which is what `dc296f3` fixes.
5. **A trailing colon in `RPATH` survives the round trip** (`f8bb3bd`). An
   RPATH ending in a colon carries a meaningful empty final entry;
   `compute_new_rpath` rebuilt the list and dropped it, so the rewritten
   search path was shorter than the one we were handed.

## What this PR must say about itself, and it is not flattering

**Three of the five have not been verified against the package that named
them.** hicolor-icon-theme has not been driven through the lazy `cc` path;
libvmaf and liblapack have not been built with the virtual-path map; no
package has been linked through the RPATH fix. Each compiles, each has the
reasoning written down, and TWO of the five carry tests. The
include-directory fix (`4e591a0`) has one regression test, verified failing
first. The RPATH fix now has nine in `patchelf.rs`, a file that had none when
`f8bb3bd` shipped - which is why `f8bb3bd` repaired one of three spellings of
its own bug and then introduced a worse one. Recounted against the tip rather
than restated: this body said ONE for several rounds after it stopped being
true. "No symptom
appeared" is the absence of one symptom, not evidence.

Say that in the PR body rather than letting a reviewer find it. The honest
framing is that these are fixes for failures we observed at distribution
scale, with the test evidence each one actually has, and the maintainer is
better placed than we are to say whether that is enough for a merge.

**Ordering against the rest of the queue.** This PR is independent of round
5's pre-PR question, which blocks the inference work and is James's to send.
It does not depend on PR #43, #26 or #56. It can go whenever James chooses to
send it, and it is the only item in this directory in that position.

## Audit

Round 1 (2026-08-29), adversarial read of this draft against ground rule 3:

- **Blocking, fixed: the draft first listed the five fixes as a flat set and
  buried the `bash` input among them.** A missing input on every task
  derivation is a different severity from a trailing colon in an RPATH, and a
  reviewer skimming a five-item list would have read the whole PR at the
  severity of its smallest member. Ordered by what a maintainer needs to see
  first.
- **Blocking, fixed: it claimed the fixes were "verified" because the tests
  pass.** One of the five carries a test, and the count was written from
  memory as "two" until the diffs were counted. Three have never been run
  against the package that produced the failure, and the commit messages for `f8bb3bd`
  and `7fb756e` say so in terms. A PR body claiming more than the commits do
  is the defect this directory exists to catch.
- Not fixed, and named rather than hidden: the fork cannot demonstrate any of
  these against a small reproducible upstream example. Each was found by a
  distribution build of hundreds of packages. Building minimal repros for 3
  and 5 would strengthen the PR and has not been done.

Round 2 (2026-08-29), begun as an attempt to write the missing repro for fix
5 and turned into a read of the fix itself. Three findings, none of which the
first round looked for, because round 1 audited the DRAFT and not the CODE it
describes:

- **Blocking: `f8bb3bd` runs `patchelf --print-rpath` twice per ELF file.**
  `fix_rpath` calls `get_raw_rpath` at `patchelf.rs:54`, and the
  `compute_new_rpath` it then calls reaches `get_rpath` at `:82`, which spawns
  the same subprocess against the same file. The two differ only in `.trim()`
  versus `.trim_end_matches('\n')`. This file runs over every output of every
  task derivation, so the duplicate is a per-output cost across the whole
  distribution, and a maintainer reading the diff will see it before we point
  at anything else in the PR. The repair is to read the raw string once and
  pass it down.
- **Blocking, and the same defect class the fix was written for:
  `get_rpath` drops EVERY empty entry, not only the trailing one.**
  `:143` filters on `!p.is_empty()`, so a leading colon and a doubled interior
  colon lose their empty element exactly as the trailing one did.
  `f8bb3bd` restores one position and leaves the others, which means the
  commit message's claim about being "faithful about a property it was never
  asked to change" is true of one spelling and false of two.
- **`crates/nix-ninja-task/src/patchelf.rs` contains zero tests**, which is
  why both of the above survived. The count in the section above is right and
  now reads differently: the one test in this batch is not merely the only
  test, it is in a different crate from the fix that most needs one.

The repro this round set out to write is also priced now, and it is not free.
Every route to testing the RPATH logic touches `crates/nix-ninja-task`, which
is inside `nix-ninja-task`'s fileset allowlist, so it re-keys the task binary
and every banked per-TU output with it. `compute_new_rpath` and `get_rpath`
are both private, so the trick that made the 2026-08-24 regression test free
- reach the same `pub` function from `crates/nix-ninja` - is not available
without an edit that itself re-keys.

That is not an argument for skipping the test. It is an argument for the rule
at the top of `CLAUDE.md`: the two repairs above and the test that would have
caught them are one batch, landed together, paying the re-key once. Landing
the test alone would pay it and still ship the duplicate subprocess.

Round 3 (2026-08-29), after `e285861` landed the repairs round 2 demanded.

- Both blocking findings are fixed. `fix_rpath` reads the RPATH once instead
  of twice, and `parse_rpath` preserves empty entries in place so all three
  spellings survive rather than the trailing one alone.
- `patchelf.rs` now carries six tests where it carried none, and they were
  verified failing first: reverting `parse_rpath` to the old filter fails four
  of the six, including the trailing case that `f8bb3bd` claimed to fix, and
  leaves the two controls passing.
- **This changes what fix 5 IS, and the PR body has to say so.** It is no
  longer "preserve a trailing colon"; it is "stop dropping empty RPATH
  entries", of which the trailing colon was one spelling. `f8bb3bd` alone
  would have been a partial fix presented as a complete one, which is exactly
  the claim this directory exists to stop. Send the two commits together or
  squash them; sending `f8bb3bd` by itself is worse than sending nothing.
- The test count in the section above HAS BEEN RECOUNTED and corrected to two.
  It said one for several rounds after it stopped being true, and a previous
  round of this audit identified it as false and left it standing, which is a
  worse failure than not noticing.

Still owed, and none of it fixed by `e285861`:

- The minimal repros for fixes 3 and 5 do not exist. The six new tests cover
  the RPATH PARSER; nothing has linked a binary through the change, and the
  lazy-`cc` fix has still never been driven by a package with zero compile
  targets.
- Fixes 1, 2 and 4 are untouched by all of this.

Status: NOT SENDABLE, and the reason has narrowed twice. Round 1 said missing
repros; round 2 said fix 5 was defective; round 3 says fix 5 is repaired and
tested at the parser but unexercised by any link, and the repros are still
absent. The honest gate now is a single small package built at
a current tip, reporting no RPATH regression. That gate has since been partly
met: `example-hello` and `example-dynamic-deps` both build and link here, and
`nix-ninja-task: Fixed RPATH for hello` appears in the log. No claim is made
about another tree's activity; when ArtNix's build produces a result it will
be recorded in that repo's own notes. That is one build away rather
than one code change away. It remains independent of every other blocker in
this directory.
