# Where this directory came from

Everything in this directory except this file is n2, by Evan Martin, taken
unmodified except for the four changes listed below. It is not original work
of this repository, and `LICENSE` (Apache-2.0) and `README.md` are n2's own.

    origin      https://github.com/evmar/n2
    taken from  https://github.com/hinshun/n2  branch feature/minimal-pub
    revision    341b5112

## Why a copy rather than a dependency

nix-ninja parses every `build.ninja` through n2, consumed as a library.
Upstream declares it as a git dependency:

    n2 = { git = "https://github.com/hinshun/n2", branch = "feature/minimal-pub" }

A branch is not a revision, so that dependency resolves to whatever the branch
points at on the day the lock is written, and n2 sits in the key of every
derivation nix-ninja emits. This fork needs four changes to n2 that are not in
any published branch, and needs the thing being built to be identified by
content rather than by a moving reference, so the tree is vendored and
declared as a path dependency instead.

It is a workspace member, which is not incidental. Until it was made one, the
parser every consumer's `build.ninja` passes through was linted by nothing and
tested by nothing while still being inside the derivation key.

## The four changes, and where they are going

Each is independent, each builds and passes n2's own suite alone, and each is
prepared as a patch to be offered upstream rather than kept here.

1. `scanner`: read the file instead of calling `set_len` on uninitialised
   bytes, which is undefined behaviour whenever the read comes up short.
2. `signal`: cast the handler through a thin pointer.
3. `pools`: make `Loader.pools` public.
4. `rspfile`: bind `${rspfile}` in commands.

Offering them, rather than carrying them, is the intended end state. Until
they land, this directory is the honest way to say what is being built.
