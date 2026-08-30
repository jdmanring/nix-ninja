# DRAFT: undefined behaviour in n2's `read_file_with_nul`

Not sent. Target is **`evmar/n2`**, not nix-ninja and not `hinshun/n2` - the
destination rule item 6 in `README.md` already established, for the reason it
established it: `hinshun/n2` has issues disabled, zero PRs in any state, and a
last push of 2025-04-23, so a PR aimed there has nowhere to land.

## What it is

`src/scanner.rs`, `read_file_with_nul`:

    let mut bytes = Vec::with_capacity(size + 1);
    unsafe {
        bytes.set_len(size);
    }
    file.read_exact(&mut bytes[..size])?;

`set_len` after `with_capacity` makes the buffer's first `size` bytes readable
by safe code while they are uninitialised, and `&mut bytes[..size]` hands them
out. That is undefined behaviour, not a lint's preference; `clippy::uninit_vec`
is a `correctness` lint and denies by default.

The intent is sound and worth preserving in the fix: `std::fs::read()` followed
by `push(0)` allocates the file's size and then grows, copying the whole file,
which is what the function's own comment says it exists to avoid.

## The fix, and why this shape

    let mut bytes = Vec::with_capacity(size + 1);
    file.read_to_end(&mut bytes)?;
    bytes.push(0);

`read_to_end` fills the capacity already reserved, so the single allocation
survives, and it initialises what it writes, so the `unsafe` block goes
entirely.

It also repairs something the original had that is easy to miss: `read_exact`
on `&mut bytes[..size]` reads EXACTLY the size taken from the earlier
`metadata()` call, so a file that grew between the two was silently truncated
and a file that shrank produced `UnexpectedEof`. `read_to_end` takes what is
actually there. Worth saying in the PR body, because it is a behaviour change
and a maintainer should get to judge it rather than discover it.

## Test

One test, covering the trailing nul, content fidelity, and the empty file -
the last because it is the case the `set_len` path was least obviously correct
for and it is a real ninja input.

## What this PR must NOT carry

This fork's `vendor-n2` diverges from its pinned base in five files as of
2026-08-29. A sixth path, `.cargo-ok`, was a committed cargo vendoring
artefact and has been removed rather than routed - it belonged in no PR:

    Cargo.toml          BOTH: our ${rspfile} liability AND our lint policy
    src/load.rs         the ${rspfile} work - ours, not upstream's problem
    src/scanner.rs      THIS fix - upstream's own bug
    src/signal.rs       function_casts_as_integer, a forward-compat cast
    tests/e2e/mod.rs    deprecated TempDir::into_path -> keep()

Only `scanner.rs` belongs in this PR - but "belongs in this PR" is a question
about batching, and every one of the five needs a route home rather than a
bucket. In a fork whose purpose is contribution, a file with no destination is
permanent divergence by default:

    src/scanner.rs      THIS PR, to evmar/n2.
    src/signal.rs       evmar/n2, as a separate small PR with the e2e change.
    tests/e2e/mod.rs    with signal.rs. Both are forward-compat modernisations
                        and neither belongs bundled with a soundness fix that
                        should be reviewable in one screen.
    src/load.rs         item 6. It is the ${rspfile} work and the reason this
                        tree vendored at all, and item 6's whole point is that
                        the vendoring should stop.
    Cargo.toml          two unrelated halves. The ${rspfile} bits go with
                        load.rs. The [lints] block goes when the vendoring
                        goes - see below, because its justification is not
                        what it first looks like.

**The `[lints]` block is not us lowering a standard, and the first draft of
this file said it was.** Measured 2026-08-29 rather than assumed: `n2` has no
`source` field in `Cargo.lock`, so it is a PATH dependency, while `harmonia`
and `igraph` are git dependencies. Cargo applies `--cap-lints allow` to
everything that is not a workspace member or a path dependency, and the
evidence is in the lint output itself - harmonia and igraph contribute ZERO
lints while vendor-n2 contributed 44.

So those 44 exist because we vendored, not because the crate is careless.
Consuming n2 the way upstream nix-ninja does - a git dependency - caps them
automatically. The `[lints]` block restores the treatment cargo would already
have given third-party code, and it is scaffolding attached to the vendoring
rather than a standing policy: when item 6 resolves and this tree stops
vendoring, the block is deleted with it and nothing is left behind.

That is worth telling the nix-ninja maintainer as part of item 6, but not as
a straight cost, because the ledger runs the other way and the honest version
is the better story:

`--cap-lints allow` silences `correctness` too. Consuming n2 as a git
dependency - which is what upstream nix-ninja does - would have hidden
`uninit_vec` permanently. The vendoring is the only reason anybody saw this
bug. So the trade is real: vendoring costs a merge liability and 44 lints of
noise, and it bought a memory-safety fix that the supported consumption path
structurally cannot surface.

## Why this one is different from everything else in this directory

Every other item here is a nix-ninja feature or fix competing for a
maintainer's attention on a roadmap they own. This is a memory-safety bug in
somebody else's crate, it is eleven lines including the test, and it is
verifiable by reading. It has the best odds of any item in this directory, and
it is the only one whose value does not depend on the maintainer agreeing with
our approach to anything.

## Audit

Round 1 (2026-08-29):

- Checked that the fix keeps the property the code exists for. It does:
  capacity is reserved once and `read_to_end` writes into it. NOT measured -
  no allocation count was taken either side, and the claim in the PR body is
  therefore about shape, not about a benchmark. Say so or say nothing.
- The truncation repair is a behaviour change riding inside a soundness fix.
  Named in the body above rather than left for review to find, which is the
  ground rule this directory runs on.
- Not established: whether `evmar/n2` is currently accepting PRs at all. Last
  push 2025-11-10 and issues enabled, read 2026-08-21, which is activity and
  not a policy. Nobody has read its contributing guidance or its open PR
  queue. That check is owed before sending and it is cheap.

Status: NOT SENDABLE, pending the two things above - a decision on how to word
the allocation claim, and a read of the target repo's PR queue and contributing
guidance. Neither is blocked on code, and this item is blocked on nothing else
in this directory.
