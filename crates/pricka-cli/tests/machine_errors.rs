use std::io::Write;
use std::process::{Command, Stdio};

fn machine_failure(args: &[&str], input: &[u8]) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pricka"));
    command.arg("--machine");
    if !args.contains(&"--bank") {
        command.args(["--bank", "seb-foretag"]);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"code\":\"sync_failed\",\"type\":\"error\"}\n"
    );
}

#[test]
fn invalid_input_and_options_fail_before_browser_or_bank_access() {
    machine_failure(&[], b"private malformed input\n");
    machine_failure(&[], b"");
    machine_failure(&[], &vec![b' '; 4097]);
    machine_failure(&["--output", "unused-output.json"], b"{}");
    machine_failure(&["--from", "2026-02-02", "--to", "2026-01-01"], b"{}");
    machine_failure(&["--bank", "handelsbanken-privat"], b"{}");
    machine_failure(&["--bank", "handelsbanken-foretag"], b"{}");
}
