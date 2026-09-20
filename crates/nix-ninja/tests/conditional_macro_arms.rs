//! A MACRO DEFINED IN SEVERAL CONDITIONAL ARMS DECLARES THE ARM MESA DOES
//! NOT TAKE (mesa 25.x, `src/mesa/glapi/shared-glapi`).
//!
//! `glapi_priv.h` defines `_GLAPI_ENTRY_ARCH_TLS_H` three times, once per
//! `#if DETECT_ARCH_*` arm, and `#include`s it. The arms are compiler
//! builtins reached through `detect_arch.h`. In the consumer's round
//! 20260920-040429 the task failed:
//!
//!     In file included from ../src/mesa/glapi/shared-glapi/core.c:8:
//!     ../src/mesa/glapi/glapi_priv.h:17:33: fatal error:
//!       glapi/entry_x86_tls.h: No such file or directory
//!
//! and the task's own `NIX_NINJA_INPUTS` carried
//! `../src/mesa/glapi/entry_ppc64le_tls.h` and NOT the x86 header the
//! compiler asked for. So the scan declared the arm for another
//! architecture.
//!
//! THE SCANNER EVALUATES NO CONDITIONAL, which is deliberate: a guarded
//! default (`#if !defined(X) / #define X "y.ch"`) is a define the walk wants
//! to FIND, not one it wants to judge, and lzo depends on that. Resolution
//! is the only filter, so an arm whose value does not resolve is dropped and
//! an arm whose value DOES resolve is declared even when the compiler's
//! branch does not take it. Mesa is the shape where the arms disagree about
//! which file exists.
//!
//! WHAT THIS FILE PINS IS THE FIX. Before it, `scan_directives` collected
//! the same-file defines into a `HashMap<String, String>`, so the LAST
//! `#define` of a name SILENTLY REPLACED the earlier ones and only the last
//! arm could ever be declared. The arms are now kept per name in file order
//! and every one that resolves is declared, which is the over-declaration
//! this walk already makes on purpose: an extra input is harmless, a missing
//! one kills the task.
//!
//! The first test below was written against the DEFECT and failed with
//! `got [..., entry_x86_tls.h, entry_ppc64le_tls.h]` after the fix, which is
//! the reading that confirms the mechanism. It now asserts the fixed
//! behaviour, so a regression to last-wins fails it.
//!
//! THIS FILE LIVES IN `crates/nix-ninja` DELIBERATELY. The scanner is in
//! `crates/deps-infer`, inside `nix-ninja-task`'s fileset, so the same test
//! written beside the code would re-key every banked PLAIN task derivation
//! to buy coverage that never runs inside a build. It reaches the same `pub`
//! function from outside the allowlist, the pattern
//! `crates/nix-ninja/tests/directory_seed_scan.rs` already sets.
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// One scratch directory per test, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("nn-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("scratch dir");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scan(cmd: &str, file: PathBuf, vp: HashMap<PathBuf, PathBuf>) -> Vec<PathBuf> {
    deps_infer::c_include_parser::retrieve_c_includes_checked(cmd, vec![file], Some(vp))
        .expect("scan")
        .includes
}

/// THE MESA SHAPE. Three arms, one file on disk, the requested arm
/// generated so only the virtual map can carry it.
///
/// The generated arm is the one gcc takes and the one that is ABSENT, which
/// is mesa exactly: `entry_x86_tls.h` is produced into the build tree while
/// `entry_ppc64le_tls.h` is checked in.
#[test]
fn every_arm_of_a_multi_define_macro_is_declared_including_the_generated_one() {
    let _s = Scratch::new("nn-cond-arms");
    let d = &_s.0;
    fs::write(d.join("core.c"), "#include \"glapi_priv.h\"\n").unwrap();
    fs::write(
        d.join("glapi_priv.h"),
        "#if defined(REALLY_INITIAL_EXEC)\n\
         #if DETECT_ARCH_X86 && MESA_SYSTEM_HAS_KMS_DRM\n\
         #define ENTRY_TLS_H \"entry_x86_tls.h\"\n\
         #elif DETECT_ARCH_PPC_64\n\
         #define ENTRY_TLS_H \"entry_ppc64le_tls.h\"\n\
         #endif\n\
         #endif\n\
         #include ENTRY_TLS_H\n",
    )
    .unwrap();
    // The arm gcc takes is generated: declared virtual, not on disk.
    let wanted = d.join("entry_x86_tls.h");
    let mut vp = HashMap::new();
    vp.insert(wanted.clone(), wanted.clone());
    // The arm for another architecture is checked in.
    fs::write(d.join("entry_ppc64le_tls.h"), "\n").unwrap();

    let got = scan(
        &format!("gcc -I{} -c core.c", d.display()),
        d.join("core.c"),
        vp,
    );

    // THE ARM GCC TAKES. Its value is generated, so nothing on disk can
    // answer for it and only keeping the arm declares it.
    assert!(
        got.contains(&wanted),
        "gcc includes entry_x86_tls.h through the x86 arm and the scan must \
         declare it even though the file is not on disk; the walk carried \
         only the last arm before the fix; got {got:?}"
    );
    // AND THE OTHER ARM IS STILL DECLARED, which is deliberate: the arms are
    // not evaluated, so the walk keeps every candidate that resolves and the
    // task carries one input it will not use. Asserted so that a later
    // "improvement" that starts choosing an arm has to say which and why.
    assert!(
        got.contains(&d.join("entry_ppc64le_tls.h")),
        "every resolving arm is declared, not only the taken one: {got:?}"
    );
}

/// THE CONTROL, AND IT IS THE LOAD-BEARING HALF. When the arm gcc takes is
/// also the arm that RESOLVES, the scan declares it and the same shape
/// builds. Without this, a scanner that declared nothing at all would pass
/// the assertion above, and the red case would be evidence about nothing.
#[test]
fn the_arm_gcc_takes_is_declared_when_it_resolves() {
    let _s = Scratch::new("nn-cond-arms-ctl");
    let d = &_s.0;
    fs::write(d.join("core.c"), "#include \"glapi_priv.h\"\n").unwrap();
    fs::write(
        d.join("glapi_priv.h"),
        "#if DETECT_ARCH_X86 && MESA_SYSTEM_HAS_KMS_DRM\n\
         #define ENTRY_TLS_H \"entry_x86_tls.h\"\n\
         #elif DETECT_ARCH_PPC_64\n\
         #define ENTRY_TLS_H \"entry_ppc64le_tls.h\"\n\
         #endif\n\
         #include ENTRY_TLS_H\n",
    )
    .unwrap();
    fs::write(d.join("entry_x86_tls.h"), "\n").unwrap();

    let got = scan(
        &format!("gcc -I{} -c core.c", d.display()),
        d.join("core.c"),
        HashMap::new(),
    );

    assert!(
        got.contains(&d.join("entry_x86_tls.h")),
        "the resolving arm must be declared: {got:?}"
    );
}

/// THE `#undef`-SEPARATED SPELLING OF THE SAME DEFECT (gperftools 2.17.2,
/// `src/stacktrace.cc`).
///
/// Mesa writes its arms as one `#if/#elif` chain. gperftools writes them as
/// a flat sequence of `#define` / `#include` / `#undef` pairs, ten of them,
/// each arm guarded by its own `#if`:
///
///     #if HAVE_DECL_BACKTRACE
///     #define STACKTRACE_INL_HEADER "stacktrace_generic-inl.h"
///     #include "stacktrace_impl_setup-inl.h"
///     #undef STACKTRACE_INL_HEADER
///     #endif
///     #ifdef HAVE_UNWIND_BACKTRACE
///     #define STACKTRACE_INL_HEADER "stacktrace_libgcc-inl.h"
///     #include "stacktrace_impl_setup-inl.h"
///     #undef STACKTRACE_INL_HEADER
///     #endif
///     ... eight more, ending on stacktrace_win32-inl.h
///
/// The consumer reported this one as "defined ONCE, a single arm", and the
/// shape it actually has decides the answer: the last arm is win32, the arm
/// gcc takes under `HAVE_UNWIND_BACKTRACE` is the libgcc one, and the fix
/// is the same. Measured on the real file, the generated libgcc header is
/// declared at `04539e8` and absent at `37672a4`.
///
/// WHY THIS IS A SEPARATE TEST. The mechanism is the same map, but the
/// failure mode a later change would introduce is different: an
/// implementation that treated `#undef` as "this name is gone" would satisfy
/// the mesa test (no `#undef` in an if/elif chain) and silently drop every
/// arm before the last here. The assertion below is that an `#undef`
/// BETWEEN arms does not clear the arms already collected.
#[test]
fn an_undef_between_arms_does_not_discard_earlier_arms() {
    let _s = Scratch::new("nn-cond-arms-undef");
    let d = &_s.0;
    fs::write(d.join("stacktrace.cc"), "#include \"setup.h\"\n").unwrap();
    fs::write(
        d.join("setup.h"),
        "#ifdef A\n\
         #define INL_H \"arm_a-inl.h\"\n\
         #include INL_H\n\
         #undef INL_H\n\
         #endif\n\
         #ifdef B\n\
         #define INL_H \"arm_b-inl.h\"\n\
         #include INL_H\n\
         #undef INL_H\n\
         #endif\n\
         #ifdef C\n\
         #define INL_H \"arm_c-inl.h\"\n\
         #include INL_H\n\
         #undef INL_H\n\
         #endif\n",
    )
    .unwrap();
    // Every arm is generated, so only the collected defines can declare any
    // of them and each `#include` is answered from the arms list alone.
    let mut vp = HashMap::new();
    for a in ["arm_a-inl.h", "arm_b-inl.h", "arm_c-inl.h"] {
        let p = d.join(a);
        vp.insert(p.clone(), p);
    }

    let got = scan(
        &format!("g++ -I{} -c stacktrace.cc", d.display()),
        d.join("stacktrace.cc"),
        vp,
    );

    for a in ["arm_a-inl.h", "arm_b-inl.h", "arm_c-inl.h"] {
        assert!(
            got.contains(&d.join(a)),
            "an arm followed by #undef is still a candidate the walk carries, \
             and resolution is the filter; {a} is missing from {got:?}"
        );
    }
}
