# How a staged item becomes a branch

Until 2026-08-30 every one of the 284 commits ahead of upstream lived on `main`
and not one staged item existed as something a PR could be opened from. The
drafts in this directory described PRs that had no branch behind them.

## The convention

    pr/<slug>      one branch per PR, branched from upstream/main, pushed to origin

`upstream/main`, never our `main`: a branch cut from `main` carries 284 commits
of unrelated work and is not reviewable.

**The fork's branch list is a published surface, and it carried four branches
that were not ours.** `feature/cmake-example`, `feature/dep-infer-clang`,
`feature/example-lix` and `feature/modules-devshell` were on
`github.com/jdmanring/nix-ninja` from the day the fork was made: GitHub's fork
operation COPIES every branch, it is not a fetch-time mirror, and a note here
that called it one was wrong. Anyone opening the fork saw four of the
maintainer's abandoned WIP branches sitting beside our work as though we were
carrying them.

Deleted from `origin` on 2026-08-30. Nothing is lost, which is the reason it
was safe: the `upstream` remote fetches them from pdtpartners, so
`upstream/feature/*` still resolves and `their-branches.md` still has its
subject. Restore any of them with

    git push origin upstream/feature/<name>:refs/heads/feature/<name>

**The rule: only branches we author belong on `origin`.** A `pr/*` branch is
ours and is meant to be seen. An inherited `feature/*` is upstream's record,
reachable through the `upstream` remote, and republishing it says something
about us that is not true.

There is no `develop` branch in this repository and nothing to cherry-pick onto.
That convention belongs to other repos; here the default branch is `main`, as
`CLAUDE.md` says.

## Author the branch, do not replay history

Our history is not the contribution. A fix worth sending is spread over commits
that also carry unrelated work, and two of the items below were rewritten mid-
history. Cherry-picking replays the mess and leaks fork-specific context into a
stranger's repository.

So: branch from `upstream/main`, take the FINAL state of the files that item
owns, and write one clean commit whose message says what a maintainer needs.

    git checkout -b pr/<slug> upstream/main
    git checkout main -- <the paths this item owns>
    # then read the diff and remove what is not this item's

**Read the staged diff before committing, every time.** `git checkout main --
<path>` takes the whole file, and our files carry more than one item. Cutting
`pr/configure-caching` this way pulled in `mkCMakePackage`, the vendor-n2
fileset entry and the CMake example, none of which is #16. Caught by reading
the diff; nothing else would have caught it.

**Strip fork-specific prose.** Comments here name ArtNix, our banked per-TU
outputs, and dated incidents in this fork. That context is why the code is
right and it is noise to a maintainer. The #16 branch's comment was rewritten
for this reason, and the #4 branch's `Cargo.toml` justification ("already in
the lock via vendor-n2's own benches") is FALSE upstream, where n2 is still a
git dependency.

Never let `docs/upstream/**` onto a PR branch. It is our staging, it discusses
the maintainer, and it is not his business.

## What exists

    pr/devstore-script      contrib/devstore.sh + CONTRIBUTING.md      item 7, gated on PR #26
    pr/bench-e2e            bench/                                     #7
    pr/bench-generate       benches/generate.rs + Cargo.toml           #4, also answers #41
    pr/configure-caching    mkMesonPackage + one example               #16

Each is one commit off `upstream/main`. None has been offered anywhere.

## What CANNOT be cut this way, and why it matters

The four above are separable because each owns files nothing else touches. The
remaining items do not, and pretending otherwise produces a branch that does
not build:

- **dependency inference** (the largest item), **#17 depfiles**, **#5 phony**,
  **the generated-header fixes**, **the bug-fix batch**: all live inside
  `crates/nix-ninja/src/task.rs` and `crates/deps-infer/src/c_include_parser.rs`,
  interleaved with each other and with work that depends on earlier changes.
  Separating them needs hunk-level surgery and a compile check per branch, not
  a path checkout.
- **the n2 scanner soundness fix** targets `evmar/n2`, a different repository.
  `vendor-n2` here is a fork of it, so the branch belongs in an n2 clone and
  cannot sit on top of `upstream/main` at all.
- **the boost patch collision** targets `NixOS/nix`, same reason.

**A branch that does not compile against `upstream/main` is worse than no
branch**, because it looks ready. None of the four above has been compiled
against upstream yet - upstream resolves `n2` from a git dependency and this
fork resolves it from `vendor-n2`, so a build there is a different resolution
than any build done here. That check is owed before any of them is offered.
