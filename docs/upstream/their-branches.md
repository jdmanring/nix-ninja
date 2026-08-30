# Their branches and their PRs, and which ones nothing here had read

Written 2026-08-30 after `git branch -r` surfaced four upstream branches no
sweep had looked at. The first version of this file drew the wrong conclusion
from them twice, and both corrections are the point of it.

## The four branches are PR head branches, not stray WIP

    upstream/feature/cmake-example      306a434  2025-04-02  -> PR #24, CLOSED
    upstream/feature/example-lix        a479e6a  2025-03-31  -> PR  #8, OPEN draft
    upstream/feature/modules-devshell   a6225a6  2025-04-18  -> the #26 work
    upstream/feature/dep-infer-clang    e4318c0  2025-08-28  -> PR #37, OPEN draft

So the branches are not a separate channel to audit. **The gap is in the PR
enumeration, and that is the finding.**

## The actual gap: issues were enumerated exhaustively, PRs never were

This directory's sweeps state that all twelve open ISSUES were enumerated by
hand. No sweep ever enumerated the open PULL REQUESTS. Checked by grep, the
drafts here reference exactly three - #26, #43, #56 - and those three came up
because something else led to them, not because anyone listed the PRs.

    open PRs: #56 #43 #42 #37 #30 #26 #8
    referenced anywhere in this directory: #56 #43 #26

**#37, #42, #30 and #8 have never been named in any document here.** An issue
list and a PR list are different queries against the same forge, and only one
of them was ever run.

## #37 is the one that matters

`wip: try using libclang for dependency inference`, elpdt852, opened
2025-08-28, **still OPEN as a draft**, zero comments, untouched since the day
it was opened.

This is the maintainer trying to REPLACE the textual scanner, and dependency
inference is our largest staged item. `CONTRIBUTING.md` names the weakness in
his own words: deps-infer "will include more headers than gcc because we match
everything instead of processing ifdef, etc." libclang is the principled answer
to exactly that.

**It does not make our work moot, and saying so would be as wrong as ignoring
it.** Most classes we fixed are not ifdef-related: nasm `%include`, non-UTF-8
sources, a directory shadowing a header on the include path, a generated header
that does not exist at scan time. libclang gets the ifdef and computed-macro
cases for free and still cannot answer the generated header, because there are
no bytes to parse either way. Several of our fixes are about WHICH files exist
and where, not about how a file is read.

What it changes is the pitch. An inference PR that does not mention #37 reads
as though we never looked at the project's own open PRs - which, until
2026-08-30, was true. One that says "here are the input classes with repros,
here is which #37 would subsume and which it would not" is a contribution to
his decision, and costs one paragraph.

That the draft has sat a year with no comment is also information: it is a
direction he tried and did not finish.

## #24 was CLOSED for a conflict, not on merit - which HELPS #20

`wip: Add CMake example`, closed 2025-04-03 with one comment from the
maintainer: **"Conflicts with #25"**. #25 (`docs/incremental-demo`) merged and
brought `examples/incremental` with it.

So his CMake example did not lose an argument, it lost a merge race, and #20 is
still open and still `help wanted` sixteen months later. `roadmap-coverage.md`
said `mkCMakePackage` is "absent from `upstream/main`, so it is ours to offer",
which is true and now better supported than when it was written.

Worth reading before offering: PR #24 changed **18 lines of
`crates/nix-ninja/src/task.rs`**, and two are defects this fork hit and fixed
independently.

    -        // TODO: If you don't find it it's ok, e.g. ./generated_binary
    -        let cmdline_path = which_store_path(&cmdline_binary)?;
    +        if let Ok(cmdline_path) = which_store_path(&cmdline_binary) {

That is "a command that is a graph output" - our seventh whole-graph class,
bought by orc's `orcc` and fixed in `15152f2`. He hit it in April 2025 and
tolerated the lookup failing; we resolve the command against the graph. Same
defect, two fixes, and independent rediscovery is the strongest evidence
available that the class is real rather than an artifact of our scale. Say so
in the PR.

The other is `if let Some(_) = file.input { continue; }`, commented "Skipping
outputs of phony targets for now" - the #5 area, and his own statement that
phony handling was incomplete then.

## THE CORRECTION: the devstore was NOT missed

The first version of this file said `devstore-pr.md` "does not mention that
somebody already tried". **That is false, and it is the kind of error this
directory exists to prevent.** `devstore-pr.md` round 2 (2026-08-21) found PR
#26, documented it at length, found the maintainer's unanswered question in
that thread, and REORDERED the whole plan because of it - `pr26-reply.md`
exists for that reason and the README's item 0 displaced item 7 on those
grounds.

`upstream/feature/modules-devshell` is that same work. Seeing a branch, not
recognising it as the head of a PR the directory already knew, and writing it
up as an unread duplicate was a failure to check before accusing.

## The rule

**Enumerate the PULL REQUESTS as a list, not incidentally.** Issues were
enumerated by hand and PRs never were, so the three PRs this directory knows
are the three that something else led it to. `gh pr list --state all` is one
command and it would have surfaced #37 a week ago.

And the corollary this file learned about itself: a branch that looks unread
may be the head of a PR already read. Map branch to PR before writing that
anyone missed anything.
