//! A recv invocation without a permit from the licence gate never reaches the radio code, and
//! with `--redact-licence` its run files never hold the licence ID or name.
//!
//! Ports named here do not exist; a run that gets past the gate fails at `open /nonexistent...`.
//! Nothing opens a real port or radio.

use moq_subghz_licence::testing::{self, TempDir};
use std::process::{Command, Output};

const PORT: &str = "/nonexistent/moq-test-port";

fn recv(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_moq-subghz-recv"))
        .args(args)
        .output()
        .unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn reached_radio(o: &Output) -> bool {
    err(o).contains(&format!("open {PORT}"))
}

#[test]
fn an_amateur_profile_without_a_licence_or_rx_amateur_never_opens_the_port() {
    let d = TempDir::new("recv-amateur");
    let out = d.path().join("run");
    let out = out.to_str().unwrap();
    let o = recv(&["--port", PORT, "--amateur", "--out", out]);
    assert!(!o.status.success());
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(
        err(&o).contains("amateur profiles need licensed mode"),
        "{}",
        err(&o)
    );
    // --transmit does not help a receiver.
    let o = recv(&["--port", PORT, "--amateur", "--transmit", "--out", out]);
    assert!(!reached_radio(&o), "{}", err(&o));
    // Positive controls: --rx-amateur, or a licence file, gets to the port.
    let o = recv(&["--port", PORT, "--amateur", "--rx-amateur", "--out", out]);
    assert!(reached_radio(&o), "{}", err(&o));
    let keys = TempDir::new("recv-key");
    let lic = keys.licence_file();
    let o = recv(&[
        "--port",
        PORT,
        "--amateur",
        "--licence-file",
        lic.to_str().unwrap(),
        "--out",
        out,
    ]);
    assert!(reached_radio(&o), "{}", err(&o));
}

#[test]
fn an_amateur_profile_refuses_encryption_before_the_port() {
    let d = TempDir::new("recv-enc");
    let out = d.path().join("run");
    let o = recv(&[
        "--port",
        PORT,
        "--amateur",
        "--rx-amateur",
        "--key-file",
        "/nonexistent/key",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(err(&o).contains("forbid encryption"), "{}", err(&o));
}

#[test]
fn an_ism_receive_run_passes_the_gate_without_any_flag() {
    let d = TempDir::new("recv-ism");
    let out = d.path().join("run");
    let o = recv(&["--port", PORT, "--out", out.to_str().unwrap()]);
    assert!(reached_radio(&o), "{}", err(&o));
}

#[test]
fn a_bad_licence_file_stops_the_run() {
    let d = TempDir::new("recv-bad");
    let out = d.path().join("run");
    let o = recv(&[
        "--licence-file",
        "/nonexistent/licence.toml",
        "--replay",
        "/nonexistent/replay.txt",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!o.status.success());
    assert!(err(&o).contains("licence"), "{}", err(&o));
}

#[test]
fn run_files_hold_the_licence_only_without_redaction() {
    let cfg = testing::licence();
    for redact in [false, true] {
        let (keys, d) = (TempDir::new("recv-key"), TempDir::new("recv-run"));
        let lic = keys.licence_file();
        let replay = d.path().join("replay.txt");
        std::fs::write(&replay, "R 1000 -90 5 deadbeef\nX junk\n").unwrap();
        let out = d.path().join("run");
        let mut args = vec![
            "--licence-file",
            lic.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--raw-log",
        ];
        if redact {
            args.push("--redact-licence");
        }
        let o = recv(&args);
        assert!(o.status.success(), "{}", err(&o));
        let mut all = TempDir::read_tree(&out);
        all.push_str(&String::from_utf8_lossy(&o.stdout));
        all.push_str(&err(&o));
        assert_eq!(cfg.appears_in(&all), !redact, "redact={redact}");
        let summary = std::fs::read_to_string(out.join("summary.json")).unwrap();
        assert!(summary.contains("\"licence\":"), "{summary}");
        assert_eq!(summary.contains("[redacted]"), redact);
    }
}

#[test]
fn an_unlicensed_run_has_no_licence_member() {
    let d = TempDir::new("recv-unlic");
    let replay = d.path().join("replay.txt");
    std::fs::write(&replay, "X junk\n").unwrap();
    let out = d.path().join("run");
    let o = recv(&[
        "--replay",
        replay.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let summary = std::fs::read_to_string(out.join("summary.json")).unwrap();
    assert!(!summary.contains("licence"), "{summary}");
}
