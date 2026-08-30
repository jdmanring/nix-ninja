# Every deviation from upstream, and whether it is staged

Taken 2026-08-30, because "is everything staged" is a claim about a COUNT and
this directory had never taken one. The offers table in `README.md` is indexed
by what we intend to send; **it was never checked against what we actually
changed.** This file is that check, and it does not come out well.

Method: `git diff --stat upstream/main..main`, every path, each mapped to a
destination and to whether a branch and a copy-pasteable draft exist. 131 files
deviate. `vendor-n2/` (a vendored fork of evmar/n2) and `docs/upstream/`
(our staging, never sent) are excluded from the table below; everything else is
listed, nothing is summarised away.

## The table

    LOC   area                                    destination        branch  draft
    ----  --------------------------------------  -----------------  ------  -----
    6558  crates/nix-ninja/src/task.rs             SEVERAL, see below   no     part
    1567  crates/nix-builder-rpc-client/src/lib.rs their #18            no      NO
    1268  crates/nix-ninja-task/src/main.rs        whole-graph classes  no      NO
    1080  crates/deps-infer/src/c_include_parser.rs their #37 / item 2  no      NO
     721  scripts/daemon-stress-bisect.py          item 1, the wedge    no     part
     663  crates/nix-ninja/src/dyndep.rs           item 10, a QUESTION  no      NO
     488  crates/nix-ninja/src/resolve_cache.rs    item 5, their #17    no     part
     369  crates/nix-ninja/src/build.rs            spread               no      NO
     303  crates/nix-ninja/src/cli.rs              spread               no      NO
     246  crates/nix-ninja-task/src/derived_file.rs their #5            no      NO
     215  crates/nix-ninja-task/src/patchelf.rs    bug-fix batch        no     yes
     199  crates/nix-ninja/benches/generate.rs     their #4            YES     yes
     195  contrib/devstore.sh                      item 7, their #26   YES     yes
     158  bench/e2e.sh                             their #7            YES     yes
     108  crates/deps-infer/src/gcc_include_parser.rs item 2            no      NO
      99  modules/flake/pkgs/mkCMakePackage        their #20            no      NO
      77  modules/flake/pkgs/mkMesonPackage        their #16           YES     yes
      56  crates/nix-ninja/tests/include_dir_shadow.rs bug-fix batch    no     yes
      48  crates/nix-ninja/src/local.rs            their #17            no     part
      40  CONTRIBUTING.md                          item 7              YES     yes
      26  modules/flake/overlays.nix               spread               part   part
      23  flake.nix                                item 12 + local      no     part
      24  crates/deps-infer/src/gcc_depfile.rs     item 2               no      NO
       9  modules/flake/examples/cmake-hello       their #20            no      NO
       2  modules/flake/packages.nix               spread               part   part

## What the count says

**Four branches exist. They carry 469 of roughly 14,400 changed lines, which is
3%.** Everything else - the driver, the task builder, the RPC client, the
inference engine - has no branch anyone could open a PR from.

**Six areas have no drafted document at all**, so there is nothing to copy and
paste even if a branch existed:

- `nix-builder-rpc-client` (+1567), which IS their #18 and which
  `roadmap-coverage.md` reports as "Complete". It is complete as CODE and has
  never had an offer written. #18 does not appear in the README's offers table.
- `nix-ninja-task/src/main.rs` (+1268), thirteen whole-graph failure classes
  (SONAME aliases, syncqt's relative paths, cmake stamp outputs). Item 2 covers
  INFERENCE classes; these are task-builder classes and are covered by nothing.
- `c_include_parser.rs` (+1080) and its two siblings, item 2, marked "not
  drafted" in the offers table since the table was written.
- `dyndep.rs` (+663), item 10, deliberately undrafted - it needs James's answer
  on whether to ask upstream first, and that is the correct state, not a gap.
- `mkCMakePackage` (+99) and its example, their #20, `help wanted` for sixteen
  months. `roadmap-coverage.md` calls it "built... ours to offer" and no draft
  was ever written.
- `build.rs` and `cli.rs`, which are spread across several items and belong to
  whichever branch carries the feature they serve.

## Why it is like this, stated plainly rather than excused

1. **The four branches are the four items whose files nothing else touches.**
   Everything else lives inside `task.rs` and `c_include_parser.rs`,
   interleaved with each other, and separating them needs hunk-level surgery
   plus a compile per branch. That is the real cost and it has not been paid.
2. **The offers table was written from intent, not from the diff.** Nobody ever
   ran `git diff --stat upstream/main..main` and asked "does every line here
   have a destination". Doing it took one command and produced six gaps.
3. **"Complete" in `roadmap-coverage.md` means the CODE works.** It was read as
   though it meant ready to offer. #18 is the clearest case: complete, measured,
   and with no document a maintainer could read.

## The order to close it in

Cheapest first, and the first two need no surgery because their files are new:

1. **their #20** - `mkCMakePackage` plus the cmake-hello example are new files.
   A branch and a draft, gated on PR #43 and #24 as `their-branches.md` says.
2. **their #18** - `nix-builder-rpc-client` is a whole crate we changed; the
   branch is a path checkout. It needs a draft written from the measurements
   already recorded (1,157 NAR uploads sent of 35,569 offered on `example-nix`).
3. **the bug-fix batch** - four fixes, a draft that exists, and files that are
   partly separable (`patchelf.rs` and the shadow test are standalone).
4. **item 2, inference** - the largest and the one that must wait for a decision
   on #37, because the pitch depends on it.
5. **`task.rs`** - last, and possibly never as one branch. It is 6,558 lines
   spanning at least eight separate offers.

**Nothing in this file is a reason to send anything.** The blocking condition in
`README.md` is unchanged: round 5's pre-PR question is James's to send.
