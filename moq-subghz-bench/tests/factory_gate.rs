//! The board side of the factory bench (`factory-send`) runs the licence gate itself, before it
//! reads the chunk file or starts the stock app. Every case here is a refusal: nothing is
//! transmitted and no radio or serial port is touched.

use std::process::Command;

fn send(args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_moq-subghz-bench"))
        .arg("factory-send")
        .arg("/nonexistent/chunks-never-read.txt")
        .args(["--app-dir", "/nonexistent/app"])
        .args(args)
        // a licence file in the environment must not matter
        .env_remove("MOQ_SUBGHZ_LICENCE")
        .output()
        .expect("run the bench binary");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn refuses_without_transmit_even_with_the_antenna_flag() {
    let (ok, e) = send(&[
        "--i-have-an-antenna",
        "--no-licence",
        "--freq-mhz",
        "915.0",
        "--power-dbm",
        "5",
    ]);
    assert!(!ok && e.contains("--transmit"), "{e}");
    assert!(!e.contains("chunks-never-read"), "{e}");
}

#[test]
fn refuses_the_stock_868_build() {
    let (ok, e) = send(&[
        "--i-have-an-antenna",
        "--transmit",
        "--no-licence",
        "--freq-mhz",
        "868.1",
        "--power-dbm",
        "17",
    ]);
    assert!(
        !ok && e.contains("868.1 MHz") && e.contains("gps-hat-bench.patch"),
        "{e}"
    );
    assert!(!e.contains("chunks-never-read"), "{e}");
}

#[test]
fn refuses_a_power_above_the_projects_cap() {
    let (ok, e) = send(&[
        "--i-have-an-antenna",
        "--transmit",
        "--no-licence",
        "--freq-mhz",
        "915.0",
        "--power-dbm",
        "17",
    ]);
    assert!(!ok && e.contains("2 to 5 dBm"), "{e}");
}

#[test]
fn refuses_without_the_antenna_flag_after_the_gate_passes() {
    let (ok, e) = send(&[
        "--transmit",
        "--no-licence",
        "--freq-mhz",
        "915.0",
        "--power-dbm",
        "5",
    ]);
    assert!(!ok && e.contains("--i-have-an-antenna"), "{e}");
    assert!(!e.contains("chunks-never-read"), "{e}");
}

#[test]
fn the_declaration_is_required() {
    let (ok, e) = send(&["--i-have-an-antenna", "--transmit", "--no-licence"]);
    assert!(!ok && e.contains("--freq-mhz"), "{e}");
}

#[test]
fn a_missing_licence_file_is_an_error_not_a_fallback() {
    let (ok, e) = send(&[
        "--i-have-an-antenna",
        "--transmit",
        "--licence-file",
        "/nonexistent/licence.toml",
        "--freq-mhz",
        "915.0",
        "--power-dbm",
        "5",
    ]);
    assert!(!ok && e.contains("licence"), "{e}");
    assert!(!e.contains("chunks-never-read"), "{e}");
}
