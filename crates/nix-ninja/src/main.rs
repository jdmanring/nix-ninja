use nix_ninja::cli;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};

/// Returning `Result` from `main` hands the error to Rust's own reporter,
/// which prints `Error:` and an unprefixed `Caused by:` chain. Every other
/// line this binary writes carries a `nix-ninja:` prefix so a consumer can
/// recover its output from an interleaved build log by grepping for it, and
/// the failure path was the one place that broke the contract: a round
/// reporting zero `nix-ninja: FAILED` had four failed tasks in it, each
/// announced by a line the grep could not match.
///
/// The token is ABORTED rather than FAILED, and the distinction is a
/// counting one: `FAILED` is emitted once per failed EDGE by the scheduler,
/// and a consumer counts those to size a round's damage. An invocation that
/// dies is one event about the whole run, so reusing the token would add one
/// phantom task per failing package to every such count.
///
/// Every line of the chain is prefixed rather than folded into one with
/// `{:#}`, because the cause chain is what names the failing operation and a
/// single line long enough to hold it is the length that gets spliced when
/// many drivers share one fd.
fn main() -> ExitCode {
    let run = hand_off_test_suite().and_then(|()| cli::run());
    let report = cli::report_pass();
    let run = run.and(report);
    match run {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("nix-ninja: ABORTED: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("nix-ninja:   caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Replaces this process with real ninja, arguments unchanged, when the build is a
/// test suite's own (`cli::test_suite_real_ninja`); returns only when it is
/// not, or with the reason the exec failed.
fn hand_off_test_suite() -> anyhow::Result<()> {
    let Some(real) = cli::test_suite_real_ninja(|k| std::env::var_os(k))? else {
        return Ok(());
    };
    eprintln!(
        "nix-ninja: PYTEST_CURRENT_TEST is set, a test suite's own build: handing it to {}",
        real.display()
    );
    let err = Command::new(&real)
        .args(std::env::args_os().skip(1))
        .env(cli::HANDED_OFF_VAR, "1")
        .exec();
    Err(anyhow::anyhow!("exec {}: {err}", real.display()))
}
