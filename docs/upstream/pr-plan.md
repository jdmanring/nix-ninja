# DRAFT PR split for nix-ninja

Not sent. The fork is 93 files and about +14,000 lines against `9a07e67`,
roughly three quarters of that being the vendored n2 tree. That is not a pull
request, it is a pile, and sending it as one guarantees it is never reviewed.
This is the split.

Regenerate the figures at send time (`git diff --shortstat 9a07e67..HEAD`)
rather than trusting the ones above: they moved twice while these drafts were
being written, and a stale count in a PR description is the first thing a
reviewer checks and the cheapest thing to get wrong.

Ordering principle: each PR must be reviewable without the next one, and must
be defensible on its own even if every later PR is rejected.

## PR 1 - dependency inference: the input classes

The largest body of work and the one with the clearest story: a sandboxed
builder fails where real ninja succeeds, every time, for the same underlying
reason. Real ninja does not sandbox, so a generator that reads a file nobody
declared still works, and its build file is never corrected. Under nix-ninja
each of those is a hard failure thousands of tasks in.

About forty commits, each one a rule, each found by a failure rather than by
design. They are NOT nine tidy classes - an earlier draft of this file said
nine and an audit against the log falsified it. Grouped by the ecosystem whose
generators needed them:

- **python import resolution** - the largest group and the one with real depth.
  A script's siblings, its sibling packages, packages those import, a directory
  spliced onto `sys.path` by the script itself, ancestor probing by chromium's
  layout, and the whole thing as a transitive closure rather than a per-script
  pass. PYTHONPATH has to carry the symlink directories, because python
  realpaths a script symlink for `sys.path[0]`.
- **GN and command-line arguments** - undeclared scripts referenced by
  commands, a directory named as an argument, `@`-files and rspfiles, an
  argument that names a graph node, relative arguments rebased against the gen
  dir, and quoted interpreter tokens (GN quotes paths, `which(1)` does not).
- **web resource pipelines** - `.grd` and `.grit` manifests and their textual
  includes, scaled images resolving through a context directory, vulcanize
  project files naming their data roots, tsconfig project references, and
  `node_modules` traveling with the script that resolves it.
- **phony and stamp expansion** - see below, and their #5.
- **jinja template siblings** - a declared `.template` implies its
  same-directory siblings, because `FileSystemLoader` is rooted at the module
  directory and the generator concatenates templates the build file never
  lists.
- **C23 `#embed`** - upstream PR 56 by amaanq, adopted rather than reinvented.

Plus a set of error-reporting fixes that belong in the same PR because they are
what made the rest findable: the error path printing its cause instead of a
multi-megabyte derivation dump that swallowed it, distinct messages for the two
no-result failure modes, and names on the last bare io errors.

**`NIX_NINJA_PASS_ENV` must be called out in the description, not discovered in
the diff.** It is an allowlist of ambient environment variables forwarded into
the task derivation, and it is the one thing in this PR a nix-minded reviewer
will rightly want to argue about, since it lets the invoking environment reach
into a build. It is an allowlist rather than a blanket pass-through, and
store-path values become inputs. State that up front with the case that needed
it. Finding it unannounced inside a large inference PR reads as smuggling, and
it would be a fair reading.

**One PR, one commit per rule**, rather than a PR per rule: they share a test
harness and one narrative, and split apart the reviewer loses the pattern. If
the maintainer wants it smaller, the seam is the grouping above.

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

Either way this PR carries ONLY the CLI target boundary: the `Vec` return, the
refusals, the dedup and sort, and the alias expansion needed to resolve a phony
NAMED AS A TARGET. The phony input-assembly rule itself belongs to PR 1, where
it is an input-assembly rule like every other rule there, and the argument
about which phony mechanism is right belongs in their #5. An earlier draft of
this file had PR 1 defer phony to PR 3 while PR 3 refused it, which left the
work owned by neither.

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
