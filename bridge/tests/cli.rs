use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn invalid_hook_payload_is_stdout_silent_and_fail_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zag-lens"))
        .args(["hook", "--harness", "codex", "--event", "SessionStart"])
        .env_remove("ZAG_LENS_DEBUG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zag-lens");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"not json")
        .expect("write hook payload");
    let output = child.wait_with_output().expect("wait for zag-lens");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn malformed_hook_command_is_stdout_silent_and_fail_open() {
    let output = Command::new(env!("CARGO_BIN_EXE_zag-lens"))
        .args(["hook", "--harness", "unknown"])
        .env_remove("ZAG_LENS_DEBUG")
        .output()
        .expect("run zag-lens");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
