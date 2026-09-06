# Design notes

> [!IMPORTANT]
> Please pre-read [dynamic derivations] as I'll assume you already understand
> Nix dynamic derivations.

I really liked what LSP did for the editor ecosystem in that it standardized
how language-specific intelligence communicated with editors so that the work
didn't have to be repeated for every language <-> editor combination.

In the same vein, since dynamic derivations is somewhat complicated to get
right for arbitrary build graphs, I think it'll be wise to standardize on a
common format like Ninja build files. We won't repeat the work for each build
system. It would be hard to convince them to accept a Nix backend, but
outputting ninja files seems like a reasonable ask.

Ninja also has an ecosystem of ninja-compatible implementations like [samurai],
[turtle], and [n2]. While looking for a ninja parser and deciding what language
to implement this in, I found out that the original author of ninja was behind
[n2], which was implemented in Rust! I quickly got to work thinking it'll have
good synergy with the Nix community's bias towards the language. In retrospect,
this may introduce bootstrapping issues if we wanted to use `nix-ninja` to
build packages like LLVM in nixpkgs. I figured I'll worry about that later or
someone more courageous will reimplement this in C99.

### Topological order

Nix derivations depending on the output of other derivations need to be
generated in topological order. This is the same as how `ninja` and `n2` work
so `nix-ninja` just follows `n2`'s implementation.

Not all ninja build files are legal, so we also should try to detect cycles and
other undocumented behavior that other ninja implementations have.

### Handling inputs & outputs

Firstly, let's talk about how to handle inputs and outputs for each build
target. My initial approach was to modify the variable scope and replace how
`n2::graph::FileId` was resolved to filenames like `../main.cpp` with one from
the nix store. Then once the rule was evaluated it would read inputs from the
nix store, and then write outputs to content addressed output placeholders.

However, not all build commands use the `$in` and `$out` implicit variables.
For example:

```build.ninja
rule CUSTOM_COMMAND
 command = $COMMAND
 description = $DESC
 restat = 1

build src/libstore/libnixstore.so.p/schema.sql.gen.hh: CUSTOM_COMMAND ../src/libstore/schema.sql | /nix/store/4k90qpzh1a4sldhnf7cxwkm9c0agq4fp-bash-interactive-5.2p37/bin/bash
 DESC = Generating$ 'src/libstore/libnixstore.so.p/schema.sql.gen.hh'
 COMMAND = /nix/store/4k90qpzh1a4sldhnf7cxwkm9c0agq4fp-bash-interactive-5.2p37/bin/bash -c '{$ echo$ '"'"'R"__NIX_STR('"'"'$ &&$ cat$ ../src/libstore/schema.sql$ &&$ echo$ '"'"')__NIX_STR"'"'"';$ }$ >$ "$$1"' _ignored_argv0 src/libstore/libnixstore.so.p/schema.sql.gen.hh
```

The build target for `src/libstore/libnixstore.so.p/schema.sql.gen.hh` sets
the `COMMAND` variable with inputs and outputs hard-coded. The command contents
should be treated as a black box, so I believe the right approach is to leave
the command alone, and have it work as-is.

Since Nix derivations will have a cache miss if their inputs change, I want
only the precise inputs the build targets need. However, they also have to
mimick the original source directory hierarchy so that relative includes will
work. So I came up with `nix-ninja-task` which is responsible for three things:

1. Prepare source directory
   - For each input, which is a Union[StorePath, SingleDerivedBuiltPath] + a
     source-mapped file path, symlink it into `$NIX_BUILD_TOP` reconstructing
     the source directory structure.

2. Run the ninja build target

3. Copy build outputs to derivation output paths
   - NOTE: In the derivation, output paths are Nix placeholders but by the time
     `nix-ninja-task` runs, these are store paths already.

#### Configure-time symlinks and what they point at

Meson and CMake create shared-library SONAME aliases with `os.symlink` at
configure time. No ninja edge produces `liborc-0.4.so.0`, and the only trace
of it in the graph is a phony that lists it as an input, so a task cannot
reach it by asking the graph. The build-directory walk records such a link as
its link TEXT and `nix-ninja-task` recreates it after input setup, where the
relative target resolves against whatever the task materialized.

Recreating the link is half of it. A link whose target is not also an input
is a dangling link, and it fails in a way that names neither the link nor the
build system:

```
ld: liblapack.so.3, needed by lib/liblapacke.so.3.12.0, not found
lib/liblapacke.so.3.12.0: undefined reference to `dgesvd_'
```

Nothing on that link's command line spells `liblapack.so.3`. It reaches the
linker through the recorded `DT_NEEDED` of a library the command does name,
and the build tree satisfies it with an alias. So a link naming a shared
library also carries the files the recorded aliases point at.

Reading each input's dynamic section would be more precise and is not
available at the point where a task is built: an input produced by another
edge is a placeholder for an output that does not exist yet, so there are no
bytes to parse. Carrying the alias targets is a graph-level answer that needs
none, at the cost of a link occasionally carrying a library it did not need.

The carry is confined to a link naming a shared library. Applying it to every
task would add inputs to every compile and every generator in a project that
has aliases at all, which changes what the driver EMITS for targets the fix
was not aimed at, and every emitted derivation is a cache key.

### Header files

Ninja build files are not intended to be written by humans, so there are many
popular build systems that target it like CMake and meson. However, when
generating these `build.ninja` files, the targets don't have header
dependencies declared. Instead, they leverage gcc to generate depfiles that it
later parses to cache the headers it processed:

```build.ninja
rule cpp_COMPILER
 command = g++ $ARGS -MD -MQ $out -MF $DEPFILE -o $out -c $in
 deps = gcc
 depfile = $DEPFILE_UNQUOTED
 description = Compiling C++ object $out
```

However, in Nix it is awkward because:

1. We don't want to include all the header files in the derivation for gcc to
   discover because then they will all contribute to the derivation input hash.

2. We don't want to compile twice or check in depfiles to source control so that
   a second derivation can be generated using dependency info from the depfiles.

I wrote two implementations in `deps-infer` crate, (1) one that runs the
ninja build target and parses the depfile, and (2) one that just scans the file
for an include regex. (2) is 6x faster than (1) as of writing this, but it's
not preprocessor aware and miss computed includes, etc. Unfortunately, unless
we write a preprocessor in Rust, we may have to stick with (1) for correctness.

### Two modes of `nix-ninja`

It is a waste to do dependency inference whenever almost any file is changed,
and generating all the derivations for compiling Nix itself does take a long
time (~2 minutes currently). There's still a lot of low hanging fruit to
optimize this but I've been thinking how we can avoid throwing that work away.

My current thoughts is that we can support two modes of running `nix-ninja`:

1. Inside a Nix derivation like mkMesonPackage producing dynamic derivations.

2. Outside a Nix derivation producing dynamic derivations and calling
   `nix build`.

In (2), we can keep depfiles and then upon a second run, `nix-ninja` can
see if the corresponding depfile exist and skip dependency inference. We can
do the same with caching generated derivations the same way regular `ninja`
tracks mtime so we can get derivation generation incrementality as well.

I'm imagining that CI in your out-of-nixpkgs repository can keep these caches
persisted betwen runs, but then inside nixpkgs it'll just use `mkMesonPackage`.

### Dependency inference on generated source files

NOTE: This is now done and we have removed `$NIX_NINJA_EXTRA_INPUTS`, please
see [dynamic dependencies] for how this is solved.

In `NixOS/nix`, `bison` is used to generate a `parser-tab.cc` and
`parser.tab.hh` file. These files include other headers so there's a need to
do dependency inference then too.

```build.ninja
build src/libexpr/parser-tab.cc src/libexpr/parser-tab.hh: CUSTOM_COMMAND ../src/libexpr/parser.y | /nix/store/p4zm691nhs5ldz5c8rfcnks7xfnjv9lb-bison-3.8.2/bin/bison
 COMMAND = /nix/store/p4zm691nhs5ldz5c8rfcnks7xfnjv9lb-bison-3.8.2/bin/bison -v -o src/libexpr/parser-tab.cc ../src/libexpr/parser.y -d
 description = Generating$ src/libexpr/parser-tab.cc$ with$ a$ custom$ command

build src/libexpr/libnixexpr.so.p/meson-generated_.._parser-tab.cc.o: cpp_COMPILER src/libexpr/parser-tab.cc || src/libexpr/lexer-tab.hh src/libexpr/libnixexpr.so.p/call-flake.nix.gen.hh src/libexpr/libnixexpr.so.p/fetchurl.nix.gen.hh src/libexpr/libnixexpr.so.p/imported-drv-to-derivation.nix.gen.hh src/libexpr/libnixexpr.so.p/primops/derivation.nix.gen.hh src/libexpr/parser-tab.hh src/libstore/libnixstore.so.p/ca-specific-schema.sql.gen.hh src/libstore/libnixstore.so.p/schema.sql.gen.hh
 DEPFILE = src/libexpr/libnixexpr.so.p/meson-generated_.._parser-tab.cc.o.d
 DEPFILE_UNQUOTED = src/libexpr/libnixexpr.so.p/meson-generated_.._parser-tab.cc.o.d
 ARGS = -Isrc/libexpr/libnixexpr.so.p -Isrc/libexpr -I../src/libexpr -Isrc/libutil -I../src/libutil -I../src/libutil/widecharwidth -Isrc/libutil/linux -I../src/libutil/linux -Isrc/libutil/unix -I../src/libutil/unix -Isrc/libstore -I../src/libstore -I../src/libstore/build -Isrc/libstore/linux -I../src/libstore/linux -Isrc/libstore/unix -I../src/libstore/unix -I../src/libstore/unix/build -Isrc/libfetchers -I../src/libfetchers -I/nix/store/c303g1m646jv9rir4zv49q1ggphsh9b5-nlohmann_json-3.11.3/include -I/nix/store/6mn1mdcvv6rgyj8q2wh5q3v0riv3z3z1-boehm-gc-8.2.8-dev/include -I/nix/store/47zbszclyyy55nb5z7dvpcki4x5g99v4-libarchive-3.7.7-dev/include -flto=auto -fdiagnostics-color=always -D_GLIBCXX_ASSERTIONS=1 -D_FILE_OFFSET_BITS=64 -Wall -Winvalid-pch -std=c++2a -O3 -include config-util.hh -include config-store.hh -include config-expr.hh -Wdeprecated-copy -Werror=suggest-override -Werror=switch -Werror=switch-enum -Werror=unused-result -Wignored-qualifiers -Wimplicit-fallthrough -Wno-deprecated-declarations -fPIC -DBOOST_CONTAINER_DYN_LINK=1 -DBOOST_CONTEXT_DYN_LINK=1 -DBOOST_ALL_NO_LIB -std=c++2a -std=c++2a -std=c++2a -pthread
```

This hasn't been implemented yet, but would require `nix-ninja` to generate a
derivation that depends on `src/libexpr/parser-tab.cc` derivation output. That
derivation needs to do dependency inference and write out another derivation
that finally runs the ninja build rule with the discovered inputs. Note that
this can be arbitrarily deep.

For now, we have a `$NIX_NINJA_EXTRA_INPUTS` hack since it's only a small part
of Nix's build graph:

```nix
nixNinjaExtraInputs = [
 "src/libexpr/libnixexpr.so.p/meson-generated_.._parser-tab.cc.o:../src/libexpr/parser.y"
 "src/libexpr/libnixexpr.so.p/meson-generated_.._lexer-tab.cc.o:../src/libexpr/parser.y"
 "src/libexpr/libnixexpr.so.p/meson-generated_.._lexer-tab.cc.o:../src/libexpr/lexer.l"
 "src/libexpr/libnixexpr.so.p/eval.cc.o:../src/libexpr/parser.y"
 "src/libexpr/libnixexpr.so.p/lexer-helpers.cc.o:../src/libexpr/parser.y"
];
```

### Keying a task on a generation rather than on the builder

This section describes a facility this fork adds; the sections around it
describe nix-ninja itself.

`nix-ninja-task`'s store path appears in every plain task derivation twice,
as the `builder` string and again among the inputs, and the driver's appears
the same way in every dynamic one. Neither is an ingredient of the result. An
ordinary compile driven at `37672a4` and at `24c94aa`, with the task binaries
those revisions build, produces the same object byte for byte, sha256
`fd0a4751af5ddbf9a9d2a3824b77b6d2fbe0dc6c71b0acf242c9764ae87ac11b`, so moving
either binary recomputes outputs the store already holds.

Content addressing does not recover it. The emitted derivations are floating
content-addressed, so early cutoff fires and stops propagation above an
unchanged output, but the cost here is at the leaves: to learn that a
compile's object is unchanged, the compile has to run.

Nix already solves this for the shell. `/bin/sh` inside a sandbox is a stable
path mapped from a store path by `sandbox-paths`, which is why upgrading
busybox re-keys nothing. Three variables apply the same treatment to the two
binaries this project puts in every key:

    NIX_NINJA_TASK_BUILDER     stable path for nix-ninja-task
    NIX_NINJA_DRIVER_BUILDER   stable path for the driver
    NIX_NINJA_TASK_ABI         the generation the key uses instead

Set, the two binaries leave both the `builder` string and the input set, and
`NIX_NINJA_TASK_ABI` is emitted into the derivation's environment in their
place, for the dynamic derivation as well as the plain one. Unset, every
emitted derivation is byte for byte what it was. A generation set on its own
is not enough and does not reach the key: with both binaries still keyed on
their store paths it would distinguish nothing, so emitting it would re-key
the whole bank for no gain. The three are read by `stable_task_builder`,
`stable_driver_builder` and `task_abi`, and `task_builder_path` and
`driver_builder_path` choose the builder.

The mapping has to reach the daemon. The nix CLI forwards
`--option extra-sandbox-paths` for a trusted user and this project's rpc
client does not, so a driver run with the variables set and no daemon-side
entry fails every task with

```
executing '/nn/bin/nix-ninja-task': No such file or directory
```

The entry maps the stable path to the store path, in the daemon's own
configuration, and the daemon has to be restarted to read it:

    extra-sandbox-paths = /nn/bin/nix-ninja-task=/nix/store/<hash>-nix-ninja-task-0.1.0/bin/nix-ninja-task

A sandbox mounts the closure of a `sandbox-paths` entry, so neither binary
needs a static build; a mapped builder finds its own glibc with no input
declared.

Two configurations are refused rather than accepted with a warning: either
builder path without `NIX_NINJA_TASK_ABI`, and either builder path spelled
without a leading slash, since a sandbox mapping names an absolute path.
Both would otherwise key every task derivation on a generation that does not
exist, and a later change to what a task writes would reuse banked outputs
from a binary that wrote something else, with nothing in the log to say so.
The refusal is raised in `builder_path`, where the derivation is built,
rather than at startup: the driver re-enters itself as `-t dynamic-task`, so
a startup check would not be the guard the key depends on.

That is the trade the generation exists to control. A task derivation stops
recording which binary built it, so two binaries that would write different
bytes become indistinguishable by key, and `NIX_NINJA_TASK_ABI` is the only
thing separating them. A change to what a task writes must move it, and pays
one re-key deliberately. A change that fixes a task which previously failed
moves nothing, because a failing task banked no output to invalidate.

An ABI-keyed bank and a builder-keyed one are otherwise indistinguishable
from a build log, so the driver names the keying it used once per process on
stderr, and says nothing when neither builder path is set
(`report_keying_once`).

### Environment variables the driver reads

These are additions this fork makes; the sections around them describe
nix-ninja itself. The three that key a task on a generation are described
above and are not repeated here.

| variable | effect |
|---|---|
| `NIX_NINJA_IMPLICIT_INPUTS_LIMIT` | The implicit-input blanket sweeps every undeclared file in the build directory into a task, and applies only while that set is no larger than this. `0` switches the blanket off, which is what a test of a discovery mechanism wants: with the blanket on, the file under test arrives whether or not the mechanism found it. Default 512. |
| `NIX_NINJA_NO_BUILD_DIR_SCAN` | Set to anything, including the empty string, to skip the build-directory walk entirely. Presence is the whole test, so `=0` also disables it. |
| `NIX_NINJA_PASS_ENV` | Whitespace-separated names to forward into every task on top of the built-in allowlist. A name that is not itself set is an error rather than a skip, because a silently dropped variable surfaces as a build failure inside a sandbox. |
| `NIX_NINJA_RESOLVE_CACHE` | `0` disables the resolve cache, `1` enables it. Unset, it is on outside a nix build and off inside one, since a sandbox starts with no cache to read. |
| `NIX_NINJA_KEEP_GOING` | `1` continues past a failed edge instead of stopping at the first. The run still exits nonzero, carrying the failure count. |
| `NIX_NINJA_ASSUME_LTO` | `1` treats a link as LTO even where no flag on the command line says so. Any other value reads as unset. |
| `NIX_NINJA_SYSTEM` | Overrides the system string emitted into task derivations when set and non-empty. |

A value that is set but cannot be read is refused rather than replaced by the
default. The distinction matters most for the blanket limit: a mistyped value
that fell back to the default ran with the blanket on, so a gate that sets it
to `0` to test a discovery mechanism would exercise the blanket instead and
report a pass either way.

The remaining `NIX_NINJA_` names in the source are not settings. They carry
the driver's own arguments into a task derivation's environment, and the task
binary reads them back inside the sandbox.

### Explicit /nix/store references

Since `meson setup build` is configuring in a Nix environment, either locally
with `nix-shell` or inside a Nix derivation, the generated `build.ninja`
usually have hard-coded references to `/nix/store` paths:

```build.ninja
build src/libutil/libnixutil.so.p/archive.cc.o: cpp_COMPILER ../src/libutil/archive.cc
 DEPFILE = src/libutil/libnixutil.so.p/archive.cc.o.d
 DEPFILE_UNQUOTED = src/libutil/libnixutil.so.p/archive.cc.o.d
 ARGS = -Isrc/libutil/libnixutil.so.p -Isrc/libutil -I../src/libutil -I../src/libutil/widecharwidth -Isrc/libutil/linux -I../src/libutil/linux -Isrc/libutil/unix -I../src/libutil/unix -I/nix/store/47zbszclyyy55nb5z7dvpcki4x5g99v4-libarchive-3.7.7-dev/include -I/nix/store/c303g1m646jv9rir4zv49q1ggphsh9b5-nlohmann_json-3.11.3/include -I/nix/store/8kcw2s7dd48vlfgy64qdlf8c9983ikg8-libblake3-1.5.5/include -I/nix/store/73cqf7hqf4mwc3pbmgkpyl473bahn3s4-openssl-3.3.2-dev/include -I/nix/store/zvdysv520xmqd4yc684c4rhvr44mvvby-libsodium-1.0.20-dev/include -I/nix/store/m69rxkn1154drqhcbnqjr6i7xbar4yb4-brotli-1.1.0-dev/include -I/nix/store/x94kpcx52cs705iwlzan0y2h9m0mqqfb-libcpuid-0.7.0/include/libcpuid -flto=auto -fdiagnostics-color=always -D_GLIBCXX_ASSERTIONS=1 -D_FILE_OFFSET_BITS=64 -Wall -Winvalid-pch -std=c++2a -O3 -include config-util.hh -Wdeprecated-copy -Werror=suggest-override -Werror=switch -Werror=switch-enum -Werror=unused-result -Wignored-qualifiers -Wimplicit-fallthrough -Wno-deprecated-declarations -fPIC -DBOOST_CONTEXT_DYN_LINK=1 -DBOOST_COROUTINES_DYN_LINK=1 -DBOOST_ALL_NO_LIB -pthread
```

In `$ARGS`, we find `-I/nix/store/47zbszclyyy55nb5z7dvpcki4x5g99v4-libarchive-3.7.7-dev/include`
which needs to be added as a derivation `inputSrc`, otherwise it the path will
be missing inside the Nix sandbox. For that we're using a regex and scan
through the evaluated `cmdline` to extract any `/nix/store` paths. Just check
that the path exists before you add it to `inputSrc`.

#### The outer derivation's own output

The existence check above is not sufficient on its own. A package that
installs headers into its own output during the build and then compiles
against them puts that output on the command line:

```
gcc -I$out/private/nss -I$out/public/nss -c ../../lib/util/utf8.c -o utf8.o
```

`$out` exists on disk at that point, so it passes the check, but it is not a
valid store path until the build that produces it finishes. Adding it as an
`inputSrc` is refused:

```
AddToStore: remote error: path '/nix/store/...-nss-3.112.5' is not valid
```

Only the objects that actually open a header there fail, while every command
in the build names the directory, which makes the failure look unrelated to
the flag that causes it.

Excluding the outer output is half an answer: the header then has no carrier
and the compiler reports it missing. The files are instead copied out of the
outer output and staged under `.nn-outer` at the output's own layout, and a
matching search path is added to the command. Both halves belong in the
driver, which runs in the outer build and can read those files. Dependency
discovery cannot do it: for a compile, discovery runs inside the task
sandbox, where the outer output does not exist.

The search paths are derived from the command line before outer output paths
are rewritten to placeholders. A command that names no directory inside its
own output produces none, and its derivation is unchanged.

### Implicit /nix/store references

Some references are to binaries like `g++` but meson generates them without
absolute paths. Unfortunately, the binary may not even be the first argument:

```build.ninja
rule STATIC_LINKER
 command = rm -f $out && ar $LINK_ARGS $out $in
 description = Linking static target $out
```

In the example above `ar` comes a store path that `meson` found in `$PATH`,
so we need to essentially `shell_words::split` and attempt to `which::which`
each one to construct the derivation's `$PATH` variable.

In my research, it seems like CMake does generate with absolute paths, so
perhaps we could add an option to `meson` upstream to generate rules with
absolute paths to binaries.

[samurai]: https://github.com/michaelforney/samurai
[turtle]: https://github.com/raviqqe/turtle-build
[n2]: https://github.com/evmar/n2
[dynamic derivations]: ./dynamic-derivations.md
[dynamic dependencies]: ./dynamic-deps.md
