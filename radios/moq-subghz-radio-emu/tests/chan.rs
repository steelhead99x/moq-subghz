//! `moq-subghz-chan` must never destroy its input or an existing output.
use std::path::PathBuf;
use std::process::{Command, Output};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("moq-chan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn chan(args: &[&std::path::Path]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_moq-subghz-chan"));
    c.args(["--link", "fsk-emu"]);
    c.arg("--in").arg(args[0]);
    c.arg("--out").arg(args[1]);
    c.output().unwrap()
}

/// Two frames of 20 bytes each, as hex.
fn capture() -> String {
    let hex = "ab".repeat(20);
    format!("R 100 0 0 {hex}\nR 200 0 0 {hex}\n")
}

fn assert_refused(o: &Output, cap: &std::path::Path) {
    assert!(!o.status.success(), "must fail: {o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("same file") || err.contains("resolves to the input"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(cap).unwrap(),
        capture(),
        "input intact"
    );
}

#[test]
fn same_path_symlink_and_hard_link_outputs_are_refused() {
    let d = dir("same");
    let cap = d.join("cap.txt");
    std::fs::write(&cap, capture()).unwrap();
    let sym = d.join("sym.txt");
    std::os::unix::fs::symlink(&cap, &sym).unwrap();
    let hard = d.join("hard.txt");
    std::fs::hard_link(&cap, &hard).unwrap();
    for out in [&cap, &sym, &hard] {
        assert_refused(&chan(&[&cap, out]), &cap);
    }
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_good_run_writes_survivors_atomically() {
    let d = dir("ok");
    let cap = d.join("cap.txt");
    let out = d.join("out.txt");
    std::fs::write(&cap, capture()).unwrap();
    std::fs::write(&out, "old contents").unwrap();
    let o = chan(&[&cap, &out]);
    assert!(o.status.success(), "{o:?}");
    let got = std::fs::read_to_string(&out).unwrap();
    assert!(got.starts_with("R "), "{got}");
    assert_eq!(got.lines().count(), 2);
    // no temporary file is left behind
    let names: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_failing_run_keeps_the_existing_output() {
    let d = dir("fail");
    let out = d.join("out.txt");
    std::fs::write(&out, "precious").unwrap();
    // the input does not exist
    let o = chan(&[&d.join("missing.txt"), &out]);
    assert!(!o.status.success());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "precious");
    // an input with no usable frames fails and is not written as an empty result
    let cap = d.join("cap.txt");
    std::fs::write(&cap, "R 1 0 0 not-hex\n").unwrap();
    let o = chan(&[&cap, &out]);
    assert!(!o.status.success(), "{o:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "precious");
    assert_eq!(
        std::fs::read_dir(&d).unwrap().count(),
        2,
        "no temporary file left"
    );
    std::fs::remove_dir_all(&d).unwrap();
}
