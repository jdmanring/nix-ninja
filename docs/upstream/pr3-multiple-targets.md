# DRAFT PR: accept multiple targets on the command line

Not sent. Sessions stage; James sends. This is item 4 in `README.md`'s order
and PR 3 in `pr-plan.md`; read both before posting, because this PR's shape
depends on how their PR 43 resolves and that state moves without telling us.

**Regenerate every figure at send time.** Nothing below carries a count, for
the reason `pr-plan.md` gives: counts against this fork are stale by
construction.

    git log --oneline 9a07e67..HEAD -- crates/nix-ninja/src/build.rs crates/nix-ninja/src/cli.rs
    git diff --shortstat 9a07e67..HEAD -- crates/nix-ninja/src/build.rs crates/nix-ninja/src/cli.rs

## Check before posting

- **re-read PR 43.** As of 2026-08-20 it was open since 2026-02-26 with zero
  comments of any kind and `mergeable_state: dirty`, so it carries conflicts
  and cannot merge without a rebase. If it has since landed, this PR is a
  rebase onto it and most of the description below is wrong;
- **credit is not optional here.** The multi-target idea and the `want_file`
  fix are RCoeurjoly's, from PR 43's own diff. Our commit `81b67e3` describes
  the `want_file` change as fixed in passing, which is wrong and would claim
  their work if a description were written from that commit message; a
  `git notes` annotation on `81b67e3` carries the correction. Address
  RCoeurjoly in the PR body, not just the maintainer.

## Description

> `ninja a b c` is ordinary usage and this driver accepts one target. The
> `TODO` for it has been in `build()` since the first commit.
>
> This is the CLI target boundary only, taken from @RCoeurjoly's #43 and
> rebased onto this fork's phony model. It deliberately does NOT bring #43's
> phony mechanism with it - that is a separate design question and belongs in
> #5, where it is already being discussed. The two changes are complementary
> and this is the half that is uncontroversial.
>
> What it does:
>
> - `targets: Vec<String>`, with an empty vector refused up front rather than
>   silently building nothing;
> - each name resolved before the scheduler runs, so an unknown target names
>   itself instead of failing later as a missing derived file;
> - **`scheduler.want_file(fid)?` rather than `let _ = scheduler.want_file(fid)`.
>   This is @RCoeurjoly's fix from #43 and not ours.** `want_file` is what
>   detects a dependency cycle, so discarding its `Result` turned the one error
>   it exists to raise into a build that proceeds and fails somewhere else;
> - outputs deduplicated by build path and sorted. Two targets legitimately
>   share outputs - ask for a phony and one of the files it aliases and the
>   file arrives twice - and a duplicate becomes a duplicate symlink operation
>   at the CLI. Sorting makes the result a function of the target SET rather
>   than of the order it was typed, which is what makes two runs comparable.
>
> One thing the sort buys that is worth stating, since it is the only
> behavioral claim here a reviewer cannot read off the diff: with the output
> order fixed by the target set, `nix-ninja a b` and `nix-ninja b a` produce
> the same output list, so a caller can diff two runs without normalizing
> first.

## What this PR does not claim

- **no performance claim.** Building several targets in one invocation shares
  a graph load and a runner where separate invocations do not, and we have not
  measured that. Do not put a number on it;
- **no phony position.** Resolving a phony NAMED AS A TARGET is in scope here
  because a target must resolve to something; how phonies should be expanded
  as INPUTS is #5's argument and is not touched;
- **no claim about #43's other files.** #43 touches eight files and adds the
  CMake example that closes #20. This PR is not a substitute for it.

## Audit

Round 1 (2026-08-21), drafted and attacked in the same pass. Findings applied:

- the first version opened by saying we needed multiple targets independently.
  That is our status, not the contributor's business, and round 5 of
  `issue-replies.md` had already rejected the identical opening in the #43
  reply. Cut - the same defect reappearing in a new file is the reason
  `~/.claude/CLAUDE.md` rule 21 exists;
- the first version stated the `want_file` fix among our changes without
  attribution, which is exactly the claim `81b67e3`'s note was written to
  prevent. It is now called out as theirs, in bold, inside the list;
- a performance sentence about sharing one graph load was cut for having no
  measurement behind it.

Not attacked by an independent reader. Rule 10 requires that before this is
sendable, and it has not happened.

Status: NOT SENDABLE. Blocked on #43's state, which must be re-read the hour
this is posted, and on the independent audit.
