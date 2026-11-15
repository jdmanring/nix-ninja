//! NINJA'S FLAGS THAT THIS DRIVER DOES NOT IMPLEMENT MUST STILL PARSE.
//!
//! An unimplemented flag made clap exit 2 having built NOTHING, and a
//! consumer that does not read the exit status takes that as an answer.
//! meson-python's `_work_to_do` is the measured case: it runs `ninja -n`,
//! captures stdout without checking the status, greps for
//! `ninja: no work to do.`, finds nothing, and rebuilds on every import.
//! Five of ninja 1.13.2's options were rejected this way; `-n` is a feature
//! and is still absent, the other four are a compatibility surface.
//!
//! This asserts the SURFACE, not the behaviour: that the driver accepts what
//! a caller may pass. What each flag then does is documented at its field.

use clap::Parser;
use nix_ninja::cli::Cli;

#[test]
fn the_compatibility_flags_parse() {
    for argv in [
        vec!["nix-ninja", "-k", "0", "target"],
        vec!["nix-ninja", "--quiet", "target"],
        vec!["nix-ninja", "-d", "stats", "target"],
        vec!["nix-ninja", "-w", "dupbuild=err", "target"],
        vec![
            "nix-ninja",
            "-k",
            "0",
            "--quiet",
            "-d",
            "stats",
            "-w",
            "dupbuild=err",
            "t",
        ],
    ] {
        assert!(
            Cli::try_parse_from(&argv).is_ok(),
            "a ninja flag this driver does not implement must not exit 2 \
             having done nothing: {argv:?}"
        );
    }
}

/// `-k`'s VALUE, which is the half this file can reach. ninja's default is
/// 1, meaning stop after the first failure; any other value asks to keep
/// going, and 0 means never stop.
///
/// WHAT THIS DOES NOT PIN: that anything READS the value. The bridge to
/// `NIX_NINJA_KEEP_GOING` sits inside `run`, past a daemon connection, so
/// deleting it leaves this test green - the shape this tree has shipped five
/// times. The wire belongs to `local/gates/ninja-flag-surface.sh`, which
/// drives the binary; asserting it here would need `run` split for the test's
/// benefit, which this project's notes refuse for a one-line decision.
#[test]
fn keep_going_carries_ninjas_semantics() {
    let default = Cli::try_parse_from(["nix-ninja", "t"]).unwrap();
    assert_eq!(default.keep_going, 1, "ninja stops after one failure");
    let zero = Cli::try_parse_from(["nix-ninja", "-k", "0", "t"]).unwrap();
    assert_eq!(zero.keep_going, 0, "-k 0 is never stop");
}

/// THE ONE THAT IS STILL MISSING, pinned so its absence is deliberate.
/// `-n` is a dry run: resolve, report what would run, build nothing. That is
/// the resolve half without the build half, and what "would run" means for
/// a driver whose outputs may substitute is a design question rather than a
/// print statement. Until it is answered, this records the gap.
#[test]
fn dry_run_is_still_unimplemented() {
    assert!(
        Cli::try_parse_from(["nix-ninja", "-n", "t"]).is_err(),
        "if -n now parses, implement the dry run and delete this test"
    );
}
