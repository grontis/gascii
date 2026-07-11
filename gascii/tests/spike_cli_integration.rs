//! Process-level test of the `--spike` CLI mode. `gascii` is a binary-only crate, so spawning
//! the compiled binary is the only way to exercise it from `gascii/tests/` — real window
//! creation, font loading, and render passes. Validates that startup-to-first-frame is logged
//! (NFR-5), the full spike matrix runs and prints a decision, and the process exits on its own.
//!
//! Runs the real 3-row x 90-frame matrix, so it takes tens of seconds — far slower than every
//! other test in this workspace. The bounded timeout turns a genuine hang into a clear failure
//! instead of an indefinitely stuck test run.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SPIKE_TIMEOUT: Duration = Duration::from_secs(90);

#[test]
fn spike_cli_runs_full_matrix_prints_decision_and_exits_cleanly() {
    let exe = env!("CARGO_BIN_EXE_gascii");
    let mut child = Command::new(exe)
        .arg("--spike")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn gascii --spike");

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            break status;
        }
        if start.elapsed() > SPIKE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "gascii --spike did not exit within {SPIKE_TIMEOUT:?} — likely a hang in the \
                 auto-close path (drive_spike_auto / ViewportCommand::Close)"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let mut stdout = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();

    assert!(
        status.success(),
        "gascii --spike exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // NFR-5's <1s target is deliberately not asserted: this is a debug binary under a test
    // harness. Only the machine-independent part of the contract — the line is emitted and
    // well-formed — is checked.
    let startup_line = stderr
        .lines()
        .find(|l| l.starts_with("startup to first frame:"))
        .unwrap_or_else(|| panic!("no NFR-5 startup line in stderr:\n{stderr}"));
    assert!(
        startup_line.contains("ms") || startup_line.ends_with('s'),
        "startup line should contain a Duration-formatted value: {startup_line}"
    );

    for label in [
        "80x25 (1:1)",
        "200x100 (fit-to-window, NFR-2 target)",
        "1024x1024 (1:1, culled)",
    ] {
        assert!(
            stdout.contains(&format!("[spike] result: {label}")),
            "missing spike result line for {label}\nstdout:\n{stdout}"
        );
    }
    assert!(stdout.contains("[spike] matrix complete"), "matrix should report completion");

    // Which decision comes out depends on machine speed / build profile, so no specific outcome
    // is pinned — only that the gate produces a recognizable one.
    let decision_line = stdout
        .lines()
        .find(|l| l.starts_with("[spike] decision: "))
        .unwrap_or_else(|| panic!("no decision line in stdout:\n{stdout}"));
    assert!(
        decision_line.contains("keep NaiveRenderer")
            || decision_line.contains("escalate to galley-cache"),
        "unexpected decision wording: {decision_line}"
    );
}
