# Their four branches, which nothing here had read

`git branch -r` has shown these since the fork was made. No sweep in this
directory looked at one until 2026-08-30, and three of the four bear directly
on what we are staging. This is the same defect the index already records
twice - offering into a place without reading the existing attempt (PR #43 for
#20, PR #26 for item 7) - and this is the third instance.

    upstream/feature/cmake-example      306a434  2025-04-02  Edgar Lee
    upstream/feature/example-lix        a479e6a  2025-03-31  Edgar Lee
    upstream/feature/modules-devshell   a6225a6  2025-04-18  Tomek Mańko
    upstream/feature/dep-infer-clang    e4318c0  2025-08-28  Edgar Lee

**All four are one commit off `main`, none is merged, and every tip PREDATES
`upstream/main`'s own tip (`9a07e67`, 2026-05-07).** So they are stale WIP, not
live work, and none of them is a reason to stop. They are statements of intent
by the maintainer about three things we intend to offer him, which is a
different and more useful thing.

## `feature/dep-infer-clang` - the one that matters most

`wip: try using libclang for dependency inference`, 181 new lines in
`crates/deps-infer/src/clang_infer.rs`.

This is the maintainer trying to REPLACE the textual scanner, and our largest
staged item (dependency-inference input classes) is a stack of fixes that make
the textual scanner correct on more inputs. `CONTRIBUTING.md` names the
weakness in his own words: deps-infer "will include more headers than gcc
because we match everything instead of processing ifdef, etc." libclang is the
principled answer to exactly that.

**It does not make our work moot, and saying so would be as wrong as ignoring
it.** The classes we fixed are mostly not ifdef-related: nasm `%include`,
non-UTF-8 sources, a directory shadowing a header on the include path, a
generated header that does not exist at scan time. A libclang parser gets the
ifdef and computed-macro cases for free and still has to answer the generated
header, because there are no bytes to parse either way. Several of our fixes
are about WHICH files exist and where, not about how a file is read.

What this changes is the PITCH. An inference PR that arrives without mentioning
this branch reads as though we have not looked at the project; one that says
"here are ten input classes with repros, here is which of them libclang would
subsume and which it would not" is a contribution to his decision. The second
is worth more and costs one paragraph.

## `feature/modules-devshell` - prior art for our item 7

`Use nix-portable to allow using the devshell without installing DD-enabled
nix globally`, by a contributor rather than the maintainer, adding
`modules/flake/pkgs/nix-portable/default.nix`.

That is the same invitation `contrib/devstore.sh` answers - CONTRIBUTING's "if
there's a good UX way of iterating on nix-ninja in a tmp store and without
modifying your main nix, please contribute!" - answered a different way, in
April 2025, and left unmerged.

`devstore-pr.md` currently claims item 7 answers "a request the maintainer
wrote down himself" and does not mention that somebody already tried. The
draft must reference this branch and say what the two approaches trade:
nix-portable ships a self-contained nix and works with no daemon setup;
devstore.sh runs the repository's OWN pinned `nix` input as a daemon, so what
you iterate against is the version the flake already builds. Neither is
obviously right, and the branch being unmerged after sixteen months is itself
information.

## `feature/cmake-example` - prior art for #20, and a fix we made independently

`wip: Add CMake example`, adding `examples/incremental/` and - the part worth
reading - **18 lines of `crates/nix-ninja/src/task.rs`**.

Two of those changes are defects this fork hit and fixed independently, which
is the strongest evidence in this file that the failure classes are real and
not artifacts of our scale:

    -        // TODO: If you don't find it it's ok, e.g. ./generated_binary
    -        let cmdline_path = which_store_path(&cmdline_binary)?;
    +        if let Ok(cmdline_path) = which_store_path(&cmdline_binary) {

That is "a command that is a graph output" - our seventh whole-graph class,
bought by orc's `orcc` and fixed in `15152f2`. **The maintainer hit it in April
2025 and left the fix on a WIP branch.** He tolerates the lookup failing; we
resolve the command against the graph. Same defect, two fixes, and ours is
reachable from a PR while his is on a branch nobody merged.

The other is `if let Some(_) = file.input { continue; }`, commented "Skipping
outputs of phony targets for now" - the #5 area, and an explicit statement that
his phony handling was incomplete at that point.

`roadmap-coverage.md` said of #20 that `mkCMakePackage` is "absent from
`upstream/main`, so it is ours to offer". True as written and misleading as
read: there is a WIP branch for the same issue, and an offer that does not
mention it repeats the PR #43 mistake in the same issue.

## `feature/example-lix`

`wip: Attempting to also build lix with nix-ninja`. Nothing here touches lix
and no staged item collides with it. Recorded so the next sweep does not have
to re-derive that it is irrelevant.

## The rule this file exists to install

**A fork's `git branch -r` is part of the upstream record, and reading only
`main`, the issues and the PRs misses it.** Every sweep in this directory
enumerated issues and pull requests, twice caught itself for not reading an
existing PR, and never ran `git branch -r` - the cheapest of the three.
