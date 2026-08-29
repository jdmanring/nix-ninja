# HANDOFF: Qwen → Claude — nix-ninja Build Failures

**Date:** 2026-08-28
**Context:** The artnix-server build with nix-ninja driving all packages (no opt-out lists) has four failure classes. This document explains what went wrong in prior sessions, what the root causes are, and what needs to be fixed.

---

## What Went Wrong (Qwen Session Failures)

1. **Unverified fixes were committed to the closure.** The passthrough hook code (`preCheckHooks+=(nnCcPassthroughOn)`, `preInstallCheckHooks+=(nnCcPassthroughOn)`) was added to `default.nix` but was **never actually deployed** — the bison build that failed with 698 test failures used the OLD nnCcHook derivation (`/nix/store/rifj462wbvwf8l64qm946blsi2k18x8n-nn-cc-hook`) which does NOT contain passthrough code. The fix was never tested on a single package before being assumed to work.

2. **An echo line was added to `nnCcPassthroughOn`**, changing the nnCcHook derivation hash and forcing a full rebuild of every cc-route package (~4400 packages). This was done without authorization or disclosure of rebuild cost. The echo line was reverted, but the damage was done — build progress was destroyed.

3. **Full closure rebuilds were forced without authorization.** Any change to the nnCcHook derivation or nix-ninja Rust code invalidates every derivation that depends on them. The user's standing rule is: NEVER change nnCcHook or nix-ninja without explicit authorization and disclosure of rebuild cost.

4. **The user's instruction to verify fixes before rolling out was repeatedly violated.** The standing development rule is: ALWAYS verify a fix on a single package before committing it to the closure. This was not done for any of the attempted fixes.

---

## Failure Class 1: bison (installCheckPhase recompiles through nix-ninja)

### Symptom
Bison's `installCheckPhase` (triggered by `doInstallCheck = 1`) recompiles C source files through the nn-cc shim and nix-ninja instead of the real compiler. This produces objects that differ from the originals, causing 698 test failures and corrupted archives.

### Root Cause (CONFIRMED)
The nnCcHook setup-hook in the derivation actually used by bison does **NOT** contain passthrough code:

```
$ cat /nix/store/rifj462wbvwf8l64qm946blsi2k18x8n-nn-cc-hook/nix-support/setup-hook
nnCcActivate() {
  export NN_NIX_NINJA=.../bin/nix-ninja
  export PATH=.../bin:$PATH
  echo "nn-cc: drop-in active"
}
postHooks+=(nnCcActivate)
```

The `nnCcPassthroughOn`, `nnCcPassthroughOff`, `preCheckHooks+=(nnCcPassthroughOn)`, and `preInstallCheckHooks+=(nnCcPassthroughOn)` lines exist in `default.nix` but are in a **different nnCcHook derivation** (`/nix/store/zk7zwwr1m9c5gwbcppa89yg48kwqh462-nn-cc-hook`) that **no derivations reference**. The bison build used the old one.

### Hook Mechanism (VERIFIED)
I traced the full Nix stdenv hook mechanism and confirmed it works correctly:
- `runHook preInstallCheck` resolves to `preInstallCheckHooks[@]` (correct)
- `postHooks+=(nnCcActivate)` resolves to `postHooks[@]` via `runHook postHook` which strips "Hook" suffix (correct)
- `preInstallCheckHooks+=(nnCcPassthroughOn)` would correctly add to the array
- The stdenv's `installCheckPhase()` calls `runHook preInstallCheck` before `make installcheck`
- The bison derivation does NOT override `installCheckPhase` as a string — it uses the stdenv default

The passthrough approach in `default.nix` is **design-correct**. The problem is purely that it was never deployed.

### Fix
The passthrough code in `default.nix` needs to be deployed. This requires rebuilding the nnCcHook derivation, which changes its hash, which invalidates all cc-route derivations (~4400 packages across the distro, ~1400 for artnix-server). **This must be authorized by the user first.** Once authorized:

1. Build just the new nnCcHook derivation
2. Build bison with the new nnCcHook: verify `NN_CC_PASSTHROUGH=1` appears in the installCheckPhase
3. Only then proceed with the full closure rebuild

### Alternative: Avoiding the nnCcHook Hash Change
If the rebuild cost is unacceptable, an alternative is to put the passthrough logic in the `nn-cc-shim.sh` script itself, detecting when it's being called during a check/installcheck phase. However, there's no reliable indicator available to the shim. The shim could check `$MAKEFLAGS` or similar, but this is fragile and untested.

### Latent Hazard: String `installCheckPhase` Overrides Defeat Hooks
`runPhase` (setup.sh line 1770) does `eval "${!curPhase:-$curPhase}"` — if a package sets `installCheckPhase` as a **string variable** (via Nix attribute), the default `installCheckPhase()` function is **completely bypassed**, and `runHook preInstallCheck` / `runHook postInstallCheck` are **never called**. This means any `preInstallCheckHooks` entries (including `nnCcPassthroughOn`) are silently skipped. ArtNix's `makefs` package (`scripts/hammer2-toolchain.nix:105`) has this pattern. Not a problem for bison (which uses the default function), but worth auditing if the passthrough approach is deployed.

---

## Failure Class 2: hicolor-icon-theme (`cc` not found at startup)

### Symptom
`Tools::new()` in `nix-ninja` (task.rs ~line 43) eagerly resolves `cc` via `which_store_path()` at startup. Packages like hicolor-icon-theme have zero compile targets, so `cc` is not on their PATH. The driver fails immediately: `cc: command not found`.

### Root Cause
Eager tool resolution in `Tools::new()`. The driver resolves all tools (cc, c++, etc.) at startup regardless of whether they'll be used. A zero-target package doesn't need `cc`.

### Fix (REQUIRES nix-ninja Rust code change → full rebuild)
Make `cc` resolution lazy: defer `which_store_path("cc")` until the first compile task actually needs it. If no compile tasks exist (zero-target build), `cc` is never resolved and the build succeeds.

**⚠️ This changes nix-ninja Rust code, which invalidates every derivation in the closure. Must be authorized.**

---

## Failure Class 3: libvmaf (`vcs_version.h` not found)

### Symptom
The include scanner (`scan_directives` in `c_include_parser.rs` ~line 395) reads `#include "vcs_version.h"` from the host filesystem. Meson's `vcs_tag()` generates this file during the build, but it doesn't exist on disk when the scanner runs pre-build. The scanner produces "Failed to read file" errors.

### Root Cause
Two interacting issues:
1. `scan_directives()` does static include analysis by reading source files from disk, but generated headers don't exist yet
2. Compile tasks are blanket-excluded from `build_dir_inputs` in task.rs (~line 1824) via the `is_gcc_task` filter, so even if the scanner could find generated headers in build inputs, they'd be excluded

### Fix (REQUIRES nix-ninja Rust code change → full rebuild)
1. Build a `virtual_paths` map from `task.inputs` (known generated file build paths) and pass it to `discover_c_includes` / `scan_directives` during static analysis, so generated headers are found without disk access
2. Reconsider the `is_gcc_task` blanket exclusion from `build_dir_inputs` — compile tasks using `deps = gcc` get their inputs from the depfile, but generated headers needed by the scanner should still be available as build inputs

**⚠️ This changes nix-ninja Rust code, which invalidates every derivation in the closure. Must be authorized.**

---

## Failure Class 4: liblapack (FortranCInterface verify failure)

### Symptom
CMake's FortranCInterface verify step fails under nix-ninja. The generated `VerifyFortran.h` sits in the build directory and is not visible to the task derivation.

### Root Cause
Same class as libvmaf: generated headers in the build directory are invisible to the nix-ninja task derivation. The `build_dir_inputs` exclusion and the static scanner's inability to resolve generated files both contribute.

### Fix
Same fix as Failure Class 3: virtual_paths for generated headers and reconsideration of `build_dir_inputs` exclusion for compile tasks.

---

## Critical Rules (DO NOT VIOLATE)

1. **NEVER change nnCcHook or nix-ninja without explicit user authorization and full disclosure of rebuild cost.** Any change to either invalidates every derivation in the closure and forces a full rebuild from scratch.

2. **ALWAYS verify a fix on a single package before rolling out to the closure.** Build the one affected package first, confirm the fix works, then proceed. Never assume a fix works.

3. **NEVER force a full closure rebuild without authorization.** The rebuild cost is days of compile time across ~4400 packages. The user must explicitly authorize this.

4. **DISCLOSE rebuild costs before making any change.** If a change touches nnCcHook or nix-ninja Rust code, state clearly: "This will force a full rebuild of ~4400 packages (all editions) / ~1400 packages (artnix-server)."

5. **ACTIONS SPEAK.** Intent is judged by actions, not words. If you force a rebuild without authorization, it doesn't matter that you "didn't mean to" — the damage is done.

---

## Current State

- **873 packages need to build** for artnix-server (nix-daemon was killed, needs restart via s6)
- The RPATH trailing colon fix from the prior session appears to work (no RPATH errors)
- The nnCcHook passthrough fix was never actually deployed (old hook used in all builds)
- 4 failure classes identified but NONE fixed (bison, hicolor-icon-theme, libvmaf, liblapack)
- Fixes for hicolor-icon-theme, libvmaf, and liblapack require nix-ninja Rust code changes
- The bison fix requires deploying the passthrough nnCcHook (which changes its hash)
- No work is authorized without explicit user direction

---

## Recommended Fix Order

1. **bison** (lowest risk): Deploy passthrough nnCcHook. Verify on bison first. Requires cc-route rebuild (~1400 packages for artnix-server). Already written, just never deployed.

2. **hicolor-icon-theme** (medium risk): Make cc resolution lazy in `Tools::new()`. Small, targeted Rust change. Requires nix-ninja rebuild → full closure rebuild.

3. **libvmaf + liblapack** (higher risk): Virtual paths for generated headers + reconsider `build_dir_inputs` exclusion. Larger Rust change. Requires nix-ninja rebuild → full closure rebuild.

4. **Combine 2 + 3** into one nix-ninja change to avoid multiple full rebuilds. Verify on one package from each class before rolling out.
