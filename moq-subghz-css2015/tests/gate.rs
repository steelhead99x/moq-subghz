//! A css2015-send invocation without a permit from the licence gate never reaches the radio
//! code, and with `--redact-licence` its output never holds the licence ID or name.
//!
//! The only radio path here is `transmit`; these tests stop at the gate (no `--transmit`, a
//! frequency outside the band) or use `--dry-run` and `--loopback`, which drive no radio. The
//! SPI device is never opened.

use moq_subghz_licence::testing::{self, TempDir};
use std::process::{Command, Output};

fn send(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_css2015-send"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn image(d: &TempDir) -> String {
    let p = d.path().join("image.jpg");
    // Only the start and end markers matter to the chunker.
    let mut jpeg = vec![0xa5u8; 600];
    jpeg[..2].copy_from_slice(&[0xff, 0xd8]);
    jpeg[598..].copy_from_slice(&[0xff, 0xd9]);
    std::fs::write(&p, jpeg).unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn without_transmit_the_radio_is_never_opened() {
    let d = TempDir::new("l15-notx");
    let f = image(&d);
    let o = send(&["--mode", "chunk", "--file", &f, "--i-have-an-antenna"]);
    let (out, err) = text(&o);
    assert!(!o.status.success());
    assert!(err.contains("pass --transmit"), "{err}");
    // It stopped before the SPI open, which would have said so.
    assert!(
        !err.contains("spidev") && !out.contains("sent frames"),
        "{err}"
    );
}

#[test]
fn without_the_antenna_flag_the_radio_is_never_opened() {
    let d = TempDir::new("l15-noant");
    let f = image(&d);
    let o = send(&["--mode", "chunk", "--file", &f, "--transmit"]);
    let (_, err) = text(&o);
    assert!(!o.status.success());
    assert!(err.contains("--i-have-an-antenna"), "{err}");
}

#[test]
fn a_bad_licence_file_stops_every_run_mode() {
    let d = TempDir::new("l15-bad");
    let f = image(&d);
    let o = send(&[
        "--mode",
        "chunk",
        "--file",
        &f,
        "--dry-run",
        "--licence-file",
        "/nonexistent/licence.toml",
    ]);
    let (_, err) = text(&o);
    assert!(!o.status.success());
    assert!(err.contains("licence"), "{err}");
}

#[test]
fn run_output_holds_the_licence_only_without_redaction() {
    let cfg = testing::licence();
    for redact in [false, true] {
        let (keys, d) = (TempDir::new("l15-key"), TempDir::new("l15-run"));
        let lic = keys.licence_file();
        let f = image(&d);
        let loop_file = d.path().join("frames.txt");
        for mode_args in [
            vec!["--dry-run".to_string()],
            vec!["--loopback".to_string(), loop_file.to_str().unwrap().into()],
        ] {
            let mut args = vec![
                "--mode",
                "chunk",
                "--file",
                &f,
                "--licence-file",
                lic.to_str().unwrap(),
            ];
            args.extend(mode_args.iter().map(String::as_str));
            if redact {
                args.push("--redact-licence");
            }
            let o = send(&args);
            let (out, err) = text(&o);
            assert!(o.status.success(), "{err}");
            let mut all = TempDir::read_tree(d.path());
            all.push_str(&out);
            all.push_str(&err);
            assert_eq!(
                cfg.appears_in(&all),
                !redact && mode_args[0] == "--dry-run",
                "redact={redact} {mode_args:?}"
            );
            if mode_args[0] == "--dry-run" {
                assert_eq!(out.contains("[redacted]"), redact);
            }
        }
    }
}

fn log(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_css2015-log"))
        .args(args)
        .output()
        .unwrap()
}

const PORT: &str = "/nonexistent/moq-test-port";

#[test]
fn log_opens_the_port_only_with_a_permit() {
    let d = TempDir::new("l15log");
    let out = d.path().join("run");
    let out = out.to_str().unwrap();
    let base = ["--mode", "chunk", "--port", PORT, "--out", out];
    let run = |extra: &[&str]| {
        let mut a = base.to_vec();
        a.extend_from_slice(extra);
        text(&log(&a)).1
    };
    // ISM receive needs no flag: the run gets as far as the port.
    assert!(run(&[]).contains(PORT));
    // Amateur profile: refused before the port without --rx-amateur or a licence.
    let e = run(&["--amateur"]);
    assert!(
        e.contains("amateur profiles need licensed mode") && !e.contains(PORT),
        "{e}"
    );
    let e = run(&["--amateur", "--transmit"]);
    assert!(!e.contains(PORT), "{e}");
    assert!(run(&["--amateur", "--rx-amateur"]).contains(PORT));
    let keys = TempDir::new("l15log-key");
    let lic = keys.licence_file();
    assert!(run(&["--amateur", "--licence-file", lic.to_str().unwrap()]).contains(PORT));
    // A bad licence file stops the run.
    let e = run(&["--licence-file", "/nonexistent/licence.toml"]);
    assert!(e.contains("licence") && !e.contains(PORT), "{e}");
}
