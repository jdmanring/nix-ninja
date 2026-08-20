# DRAFT PR split for nix-ninja

Not sent. The fork is 88 files and +13,094 lines against `9a07e67`. That is not
a pull request, it is a pile, and sending it as one guarantees it is never
reviewed. This is the split.

Ordering principle: each PR must be reviewable without the next one, and must
be defensible on its own even if every later PR is rejected.

## PR 1 - dependency inference: the input classes

The largest body of work and the one with the clearest story: a sandboxed
builder fails where real ninja succeeds, every time, for the same underlying
reason. Real ninja does not sandbox, so a generator that reads a file nobody
declared still works, and its build file is never corrected. Under nix-ninja
each of those is a hard failure thousands of tasks in.

Nine classes so far, each found by a failure and fixed by a rule:

- python import closures (a generator importing from its own subtree via a
  `sys.path.insert` assembled two statements earlier);
- GN script siblings;
- `.grd` / `.grit` resource trees;
- `node_modules`;
- stamp and phony expansion (see PR 3);
- `@`-file and rspfile arguments;
- schema directories;
- jinja template siblings: a declared `.template` input implies its
  same-directory siblings, because `FileSystemLoader` is rooted at the module
  directory and the generator concatenates templates the build file does not
  list;
- C23 `#embed`, which is upstream PR 56, adopted rather than reinvented.

**Each class should be a separate commit inside one PR**, not separate PRs:
they share a test harness and a single narrative, and split across nine PRs the
reviewer loses the pattern. If the maintainer prefers them split, the natural
seam is per-generator-ecosystem (python, GN, web resources).

The one thing to say plainly in the description: these rules are heuristics
recovering information the build file failed to declare. They are not a general
solution, and the general solution is depfiles (their #17). Say that first, or
a reviewer will say it for us.

## PR 2 - driver performance

Two commits, both in `deps-infer`, both with a profiler number:

1. keyed virtual-path lookup instead of the pairwise scan;
2. `FxHash` on the include-scan maps.

Small, self-contained, and answers their open #7 and #4. Post the numbers into
those issues FIRST (`issue-replies.md`) and open this PR only if the response is
warm. An unsolicited perf PR reads as a critique; a perf PR that answers a
maintainer's own open question reads as help.

**Send this separately from PR 1** even though both touch `deps-infer`: PR 1 is
a behavior change with a long argument, PR 2 is a pure speedup with two
measurements. Bundling them means the speedup waits on the argument.

## PR 3 - multiple CLI targets

Depends on how their PR 43 resolves, so it is written to compose either way.

If 43 lands first: rebase, keep only our phony-expansion at the target
boundary, drop the rest. If 43 stalls (open since 2026-02, no maintainer
comment as of 2026-08-20): offer the multi-target half on its own, crediting
that PR, and leave the phony mechanism question to their #5.

Either way this PR should NOT carry our phony design with it. That is a
separate argument and it belongs in the issue, not smuggled into a
target-handling change.

## PR 4 - daemon resilience

Held back deliberately until the NixOS/nix issue has an answer. It is a
workaround for someone else's bug, and a workaround merged before the bug is
understood is a workaround nobody can ever remove. If the nix side says "that
is lock X, fixed in 2.36", this PR should not exist.

If it does go: the bounded wait with escalation, the connection-drop recovery,
the two-permit retry gate, and the named `DaemonStalled` error. The two
self-inflicted failures are worth keeping in the description, because they are
the reusable part - a recovery path needs the same fault tolerance as the path
it recovers, since its own action is a load spike.

## Not a nix-ninja PR at all

`${rspfile}` support belongs in `hinshun/n2`. Sending our vendored n2 tree here
would transfer our own merge liability to them; see `README.md` in this
directory.
