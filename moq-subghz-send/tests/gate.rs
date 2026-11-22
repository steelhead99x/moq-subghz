//! A send invocation without a permit from the licence gate never reaches the radio code.
//!
//! Every case names a serial port that does not exist. If the gate lets a run through, the next
//! thing the tool does is open that port, and the error says so (`open /nonexistent...`). No
//! test opens a real port or key a radio. The Pi HAT path is only tried without `--transmit`, to
//! see the refusal.

use moq_subghz_licence::testing::{self, TempDir};
use std::process::{Command, Output};

const PORT: &str = "/nonexistent/moq-test-port";

fn send(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_moq-subghz-send"))
        .args(args)
        .output()
        .unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The tool reached the code that opens the radio.
fn reached_radio(o: &Output) -> bool {
    err(o).contains(&format!("open {PORT}"))
}

#[test]
fn without_transmit_the_radio_is_never_opened() {
    let o = send(&["--catwan", PORT, "--i-have-an-antenna"]);
    assert!(!o.status.success());
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(err(&o).contains("pass --transmit"), "{}", err(&o));
}

#[test]
fn without_the_antenna_flag_the_radio_is_never_opened() {
    let o = send(&["--catwan", PORT, "--transmit"]);
    assert!(!o.status.success());
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(err(&o).contains("--i-have-an-antenna"), "{}", err(&o));
}

#[test]
fn with_both_flags_the_gate_lets_the_run_reach_the_port() {
    // The positive control for the tests above: the same command with --transmit gets to the
    // port open, which fails because the port does not exist.
    let o = send(&["--catwan", PORT, "--i-have-an-antenna", "--transmit"]);
    assert!(!o.status.success());
    assert!(reached_radio(&o), "{}", err(&o));
}

#[test]
fn the_gate_refuses_a_span_outside_the_band_before_the_radio() {
    // 902.1 MHz passes the old carrier check, but the 500 kHz wide signal leaves the band.
    let o = send(&[
        "--catwan",
        PORT,
        "--i-have-an-antenna",
        "--transmit",
        "--freq",
        "902.1",
    ]);
    assert!(!o.status.success());
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(
        err(&o).contains("not inside a listed US band"),
        "{}",
        err(&o)
    );
}

#[test]
fn a_bad_licence_file_stops_the_run_whatever_the_link() {
    let d = TempDir::new("send-bad");
    let missing = d.path().join("missing.toml");
    for extra in [
        &["--catwan", PORT, "--i-have-an-antenna", "--transmit"][..],
        &["--loopback", "/nonexistent/dir/out.txt"][..],
    ] {
        let mut args = vec!["--licence-file", missing.to_str().unwrap()];
        args.extend_from_slice(extra);
        let o = send(&args);
        assert!(!o.status.success());
        assert!(!reached_radio(&o), "{}", err(&o));
        assert!(err(&o).contains("licence"), "{}", err(&o));
    }
}

#[test]
fn licensed_mode_with_a_good_file_still_needs_transmit() {
    let d = TempDir::new("send-lic");
    let lic = d.licence_file();
    let lic = lic.to_str().unwrap();
    let o = send(&[
        "--licence-file",
        lic,
        "--catwan",
        PORT,
        "--i-have-an-antenna",
    ]);
    assert!(!reached_radio(&o), "{}", err(&o));
    assert!(err(&o).contains("pass --transmit"), "{}", err(&o));
    // With --transmit a licensed ISM run passes the gate (no station ID is needed on ISM).
    let o = send(&[
        "--licence-file",
        lic,
        "--catwan",
        PORT,
        "--i-have-an-antenna",
        "--transmit",
    ]);
    assert!(reached_radio(&o), "{}", err(&o));
    assert!(!err(&o).contains(testing::ID) && !err(&o).contains(testing::NAME));
}

#[cfg(target_os = "linux")]
#[test]
fn the_pi_hat_is_not_opened_without_transmit() {
    let o = send(&["--i-have-an-antenna"]);
    assert!(!o.status.success());
    let e = err(&o);
    assert!(e.contains("pass --transmit"), "{e}");
    assert!(!e.contains("spidev"), "{e}");
}

/// A whole loopback run needs ffmpeg; skip (loudly) where it is missing. The recv and
/// lora2015-send tests cover the same scan without it.
#[test]
fn loopback_run_scan_for_the_licence_with_redaction() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg is not installed: skipping the send run-log scan");
        return;
    }
    let cfg = testing::licence();
    for redact in [false, true] {
        // The licence file lives in its own directory so the scan of the run's files does not
        // find the file itself.
        let (keys, d) = (TempDir::new("send-key"), TempDir::new("send-run"));
        let lic = keys.licence_file();
        let out = d.path().join("frames.txt");
        let log = d.path().join("send.jsonl");
        let mut args = vec![
            "--licence-file",
            lic.to_str().unwrap(),
            "--loopback",
            out.to_str().unwrap(),
            "--log",
            log.to_str().unwrap(),
            "--secs",
            "2",
        ];
        if redact {
            args.push("--redact-licence");
        }
        let o = send(&args);
        assert!(o.status.success(), "{}", err(&o));
        let mut all = TempDir::read_tree(d.path());
        all.push_str(&String::from_utf8_lossy(&o.stdout));
        all.push_str(&err(&o));
        assert_eq!(cfg.appears_in(&all), !redact, "redact={redact}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).contains("[redacted]"),
            redact
        );
    }
}
