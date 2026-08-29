# The ArtNix opt-out list: thirteen packages, six families

ArtNix keeps a list of packages nix-ninja does not drive (`nnOptOut` in
`site/base/default.nix`). Every entry costs that package's per-TU
resumability, which is the whole reason this tool exists, so the list is a
defect record and not a configuration choice. It is worked down, never
extended for convenience.

Opened 2026-08-29. Four entries were retired that day (bison,
hicolor-icon-theme, libvmaf, liblapack) and are not repeated here; the
thirteen below are what remains.

## SIGNATURES RECOVERED 2026-08-29, AND THE FAMILIES BELOW ARE WRONG

The list said five packages had no surviving signature and were recoverable
only by another `--keep-going` pass. That was wrong, and the error was
stopping at "the round log is gone" instead of asking what else records a
failed build. **Nix keeps a per-derivation log for every build it runs**, at
`/nix/var/log/nix/drvs/<first-two>/<rest>.drv.bz2`. All twelve packages have
them. No rebuild was needed for any of this.

Where a task fails inside a resolved derivation, the outer log names the inner
`.drv` and the real cause is in the INNER log. Chase the hash; the outer error
is routing information, not a diagnosis.

What the logs actually say, against what this file believed:

| package | believed | ACTUAL signature |
|---|---|---|
| onetbb | undeclared source input | `CMake Error: File .../integration/linux/env/vars.sh.in does not exist` |
| svt-av1 | undeclared source input | task for generated `Source-Lib-Codec-EbVersion.h` fails |
| dav1d | meson depfile, missing `.obj.ndep` | `Failed to read file include/vcs_version.h: No such file (os error 2)` |
| p11-kit | its own signature | `Failed to read file common/pkix.asn.h: No such file (os error 2)` |
| valgrind | its own signature | `mallinfo.c:5:10: fatal error: ../config.h: No such file` |
| openblas | blanket off past the 512 limit | `cannot register realisation ... because it lacks a signature by a trusted key` |
| openh264 | its own signature | `read_dir() for python closure: No such file (os error 2)` |
| x265 | its own signature | `ld.bfd: cannot find -lx265-10` / `-lx265-12` |
| libssh | its own signature | UNRECOVERED - the surviving logs carry no cause |
| c-ares, wildmidi, corrosion | as recorded | UNRECOVERED - opted out, so their recent logs are SUCCESSES |

### Three consequences, and the first is the valuable one

**The generated-header family is much larger than one entry.** dav1d, p11-kit
and valgrind all fail on a header that does not exist when the scanner reads
it, which is the same defect as libvmaf and liblapack - and that already has a
fix, the `virtual_paths` map in `2f7b8f2` with `dc296f3`. svt-av1's
`EbVersion.h` is the same shape one level out. So up to five more entries may
retire with NO NEW CODE, on a fix that is already on `main` and was sitting
discarded in a dangling commit until 2026-08-29.
Test them first. That is the cheapest work available on this list.
valgrind adds one twist worth keeping: its include is spelled `../config.h`,
so whatever resolves virtual paths has to handle a dotdot-relative spelling
rather than only a plain build-relative one.

**openblas is not a nix-ninja defect at all.** `cannot register realisation
... lacks a signature by a trusted key` is the daemon refusing a
content-addressed realisation, which is trust configuration and not dependency
discovery. The 8,299-inputs-against-a-512-limit reading was inferred from the
blanket's own log line, which prints on every build and says nothing about why
this one failed. Do not spend a scanner rewrite on it. Retest it after the
trust question is settled, and if the limit turns out to matter it will say so
with a different error.

**x265 is a new family nobody had named**: a link edge whose sibling libraries
are not among its inputs. x265 builds 8, 10 and 12-bit variants and links them
together, and `ld` cannot find `-lx265-10` or `-lx265-12`. That is undeclared
LINK inputs from sibling ninja edges, which is a different problem from
undeclared header inputs and is not addressed by anything in flight.

**openh264** stands alone: `read_dir()` on a python closure path that does not
exist, inside the upload path rather than the scanner.

### What remains genuinely unrecovered

`libssh`, `c-ares`, `wildmidi`, `corrosion`. The first has logs with no cause
in them; the other three are opted out, so every log they have is of a
successful ordinary build. These four, and only these four, need a
`--keep-going` pass to characterise.

## Working rules for this list

- One family at a time, with a test that fails before the fix.
- Verify against the actual package, not only a unit test. A family is
  retired when its packages build, and the entry comes out of `nnOptOut` in
  the same change.
- A package that fails again comes back with its FRESH signature recorded
  here. That is a better record than a name in a list.
- Mind the cost class before editing: `crates/nix-ninja` is outside
  `nix-ninja-task`'s src allowlist, but an edit to what the driver EMITS
  re-keys every banked output anyway. `CLAUDE.md` has both tests.
