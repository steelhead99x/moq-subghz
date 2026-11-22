//! `bench`, `bench-051` and `clocklog`: the matrices of runs and the clock logger, ports of
//! `scripts/bench.sh`, `scripts/bench-051.sh` and `scripts/clocklog.sh`. A failed run is counted
//! and the matrix goes on, but it ends `BENCH_FAILED: <n> run(s) failed` with exit 1.

use crate::exec::{Host, Plan, Real};
use crate::orch::{forward, lora, reverse, Cfg, Lora, Run};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Which key file a run's receiver reads: the laptop's copy or the Pi's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Laptop,
    Pi,
}

/// One run of a matrix, as data, so the plan can be checked without running anything.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Pi to laptop (`run`)
    Fwd {
        name: String,
        profile: String,
        secs: u32,
        rx_key: Option<Key>,
        args: Vec<String>,
    },
    /// Laptop to Pi (`rev`)
    Rev {
        name: String,
        profile: String,
        secs: u32,
        rx_key: Option<Key>,
        args: Vec<String>,
    },
    /// One picture over lora2015
    Lora {
        name: String,
        mode: String,
        file: String,
        profile: String,
        passes: u32,
        args: Vec<String>,
    },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Fwd { name, .. } | Step::Rev { name, .. } | Step::Lora { name, .. } => name,
        }
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// The matched rind off/on matrix of the README: both directions, 300k and 350k(x), FEC k=8.
pub fn bench_plan(secs: u32, lkey: &str, pkey: &str) -> Vec<Step> {
    let mut v = Vec::new();
    for br in [300, 350] {
        let (fp, rp) = if br == 300 {
            ("300000:100000:6", "300000:100000:12")
        } else {
            ("350000:100000:6:x", "350000:100000:14:x")
        };
        let fec = ["--fec-k", "8"];
        let with = |k: &str| {
            fec.iter()
                .map(|x| x.to_string())
                .chain(["--key-file".to_string(), k.to_string()])
                .collect::<Vec<_>>()
        };
        let f = |tag: &str, key: Option<Key>, args: Vec<String>| Step::Fwd {
            name: format!("f{br}_{tag}"),
            profile: fp.into(),
            secs,
            rx_key: key,
            args,
        };
        let r = |tag: &str, key: Option<Key>, args: Vec<String>| Step::Rev {
            name: format!("r{br}_{tag}"),
            profile: rp.into(),
            secs,
            rx_key: key,
            args,
        };
        v.push(f("off", None, s(&fec)));
        v.push(f("on", Some(Key::Laptop), with(pkey)));
        v.push(r("off", None, s(&fec)));
        v.push(r("on", Some(Key::Pi), with(lkey)));
    }
    v
}

/// Which part of the stick 0.5.1 matrix to run.
#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum Part {
    /// Live FSK video runs with FEC, plus the two extra FSK runs
    Fsk,
    /// Only the two extra FSK runs (no FEC, and with rind)
    FskExtra,
    /// The lora2015 pictures
    Lora,
    All,
}

/// The over-the-air matrix of `results/stick-0.5.1/`.
pub fn plan_051(part: Part, reps: u32, fsk_secs: u32, pkey: &str) -> Vec<Step> {
    let p300 = "300000:100000:6";
    let fwd = |name: &str, key: Option<Key>, args: &[&str]| Step::Fwd {
        name: name.into(),
        profile: p300.into(),
        secs: fsk_secs,
        rx_key: key,
        args: s(args),
    };
    let mut v = Vec::new();
    if matches!(part, Part::Fsk | Part::All) {
        for i in 1..=4 {
            v.push(fwd(&format!("f300-fec8-p5-{i}"), None, &["--fec-k", "8"]));
        }
        for i in 1..=2 {
            v.push(fwd(
                &format!("f300-fec8-p2-{i}"),
                None,
                &["--fec-k", "8", "--power", "2"],
            ));
        }
    }
    if matches!(part, Part::FskExtra | Part::Fsk | Part::All) {
        v.push(fwd("f300-nofec-p5-1", None, &[]));
        v.push(fwd(
            "f300-rind-fec8-p5-1",
            Some(Key::Laptop),
            &["--fec-k", "8", "--key-file", pkey],
        ));
    }
    if matches!(part, Part::Lora | Part::All) {
        for rep in 1..=reps {
            for prof in ["sf7-125", "sf7-250", "sf7-500", "sf8-500"] {
                let dr = if prof == "sf8-500" { "dr4" } else { "dr3" };
                for size in ["small", "large"] {
                    let (jpg, ssdv) = if size == "small" {
                        ("f160x120-q20.jpg", "f160x112-q20.ssdv")
                    } else {
                        ("f320x240-q10.jpg", "f320x240-q10.ssdv")
                    };
                    for (mode, file, args) in [
                        ("chunk", jpg, vec![]),
                        ("ssdv", ssdv, vec![]),
                        ("lorawan", jpg, s(&["--dr", dr])),
                    ] {
                        v.push(Step::Lora {
                            name: format!("{mode}-{prof}-{size}-r{rep}"),
                            mode: mode.into(),
                            file: file.into(),
                            profile: prof.into(),
                            passes: 3,
                            args,
                        });
                    }
                }
            }
        }
    }
    v
}

/// Where the key files are: the laptop's path and the path of the same file on the Pi.
#[derive(Clone, Debug)]
pub struct Keys {
    pub lkey: String,
    pub pkey: String,
}

fn run_step(h: &mut dyn Host, c: &Cfg, play: bool, k: &Keys, st: &Step) -> Result<()> {
    let key = |x: &Option<Key>| {
        x.map(|x| {
            if x == Key::Laptop {
                k.lkey.as_str()
            } else {
                k.pkey.as_str()
            }
        })
    };
    match st {
        Step::Fwd {
            name,
            profile,
            secs,
            rx_key,
            args,
        } => forward(
            h,
            c,
            &Run {
                name,
                profile,
                secs: *secs,
                rx_key: key(rx_key),
                rx: "",
                play,
                args,
            },
        ),
        Step::Rev {
            name,
            profile,
            secs,
            rx_key,
            args,
        } => reverse(
            h,
            c,
            &Run {
                name,
                profile,
                secs: *secs,
                rx_key: key(rx_key),
                rx: "",
                play: false,
                args,
            },
        ),
        Step::Lora {
            name,
            mode,
            file,
            profile,
            passes,
            args,
        } => lora(
            h,
            c,
            &Lora {
                name,
                mode,
                file,
                profile,
                passes: *passes,
                args,
            },
        ),
    }
}

/// Run every step, keep going after a failure, and fail at the end if any failed.
pub fn run_matrix(
    h: &mut dyn Host,
    c: &Cfg,
    play: bool,
    k: &Keys,
    steps: &[Step],
    say: &mut dyn FnMut(&str),
) -> Result<()> {
    let mut fails = 0;
    for st in steps {
        say(&format!("{} {}", kind(st), st.name()));
        if let Err(e) = run_step(h, c, play, k, st) {
            fails += 1;
            say(&format!("FAILED: {e:#}"));
        }
    }
    if fails > 0 {
        bail!("BENCH_FAILED: {fails} run(s) failed");
    }
    say("BENCH_DONE");
    Ok(())
}

fn kind(st: &Step) -> &'static str {
    match st {
        Step::Fwd { .. } => "fsk fwd",
        Step::Rev { .. } => "fsk rev",
        Step::Lora { .. } => "lora",
    }
}

/// `YYYY-MM-DD HH:MM:SS` in local time, like `date +%F\ %T`.
// libc's time_t is deprecated on musl while it is still 32 bit; only the laptop calls this.
#[allow(deprecated)]
pub fn stamp_now() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: localtime_r and strftime on a zeroed tm and a buffer of the stated size.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let mut buf = [0u8; 32];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%F %T".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// One line of `clock_log.jsonl`: the laptop's unix time in microseconds and a clock check.
pub fn clock_line(h: &mut dyn Host, c: &Cfg, t_us: u128) -> Result<Option<String>> {
    let p = Plan::new(c.bin("moq-subghz-recv"), ["--clock-check", &c.pi]).err("/dev/null");
    let d = h.run(&p)?;
    let out = d.stdout.trim();
    if d.code != 0 || serde_json::from_str::<serde_json::Value>(out).is_err() {
        return Ok(None); // a missed check is a gap in the log, not a failed run
    }
    Ok(Some(format!("{{\"t_us\":{t_us},\"c\":{out}}}")))
}

/// Log the Pi-minus-laptop clock offset every 5 s until `stop` says so.
pub fn clock_log(c: &Cfg, stop: &dyn Fn() -> bool) -> Result<()> {
    std::fs::create_dir_all(&c.runs)?;
    let path = c.runs.join("clock_log.jsonl");
    let mut h = Real::default();
    while !stop() {
        let t = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
        if let Some(line) = clock_line(&mut h, c, t)? {
            let mut f = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .with_context(|| format!("open {}", path.display()))?;
            writeln!(f, "{line}")?;
        }
        for _ in 0..50 {
            if stop() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// Run `body` with the clock logger going in the background, and stop it after.
pub fn with_clock_log<T>(c: &Cfg, dry: bool, body: impl FnOnce() -> T) -> T {
    if dry {
        return body();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (c2, s2) = (c.clone(), stop.clone());
    let t = std::thread::spawn(move || clock_log(&c2, &|| s2.load(Ordering::SeqCst)));
    let out = body();
    stop.store(true, Ordering::SeqCst);
    if let Ok(Err(e)) = t.join() {
        eprintln!("clock log stopped: {e:#}");
    }
    out
}

/// `clocklog` on its own: until `<runs>/clocklog.stop` exists.
pub fn clocklog_until_file(c: &Cfg) -> Result<()> {
    let stop = c.runs.join("clocklog.stop");
    let _ = std::fs::remove_file(&stop);
    clock_log(c, &|| Path::new(&stop).exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orch::fake::Fake;
    use crate::testutil::Scratch;

    #[test]
    fn bench_is_the_eight_run_matrix() {
        let p = bench_plan(330, "L.hex", "P.hex");
        let names: Vec<&str> = p.iter().map(Step::name).collect();
        assert_eq!(
            names,
            [
                "f300_off", "f300_on", "r300_off", "r300_on", "f350_off", "f350_on", "r350_off",
                "r350_on"
            ]
        );
        assert_eq!(
            p[1],
            Step::Fwd {
                name: "f300_on".into(),
                profile: "300000:100000:6".into(),
                secs: 330,
                rx_key: Some(Key::Laptop),
                args: s(&["--fec-k", "8", "--key-file", "P.hex"])
            }
        );
        assert_eq!(
            p[7],
            Step::Rev {
                name: "r350_on".into(),
                profile: "350000:100000:14:x".into(),
                secs: 330,
                rx_key: Some(Key::Pi),
                args: s(&["--fec-k", "8", "--key-file", "L.hex"])
            }
        );
    }

    #[test]
    fn stick_matrix_sizes() {
        assert_eq!(plan_051(Part::Fsk, 3, 120, "k").len(), 8);
        assert_eq!(plan_051(Part::FskExtra, 3, 120, "k").len(), 2);
        assert_eq!(plan_051(Part::Lora, 3, 120, "k").len(), 72);
        assert_eq!(plan_051(Part::All, 3, 120, "k").len(), 80);
        assert_eq!(plan_051(Part::Lora, 1, 120, "k").len(), 24);
        let l = plan_051(Part::Lora, 1, 120, "k");
        assert_eq!(
            l[0],
            Step::Lora {
                name: "chunk-sf7-125-small-r1".into(),
                mode: "chunk".into(),
                file: "f160x120-q20.jpg".into(),
                profile: "sf7-125".into(),
                passes: 3,
                args: vec![]
            }
        );
        let last = l.last().unwrap();
        assert_eq!(last.name(), "lorawan-sf8-500-large-r1");
        assert!(
            matches!(last, Step::Lora { args, file, .. } if args == &s(&["--dr", "dr4"]) && file == "f320x240-q10.jpg")
        );
        let a = plan_051(Part::All, 1, 120, "k");
        assert_eq!(a[0].name(), "f300-fec8-p5-1");
        assert!(
            matches!(&a[4], Step::Fwd { args, .. } if args == &s(&["--fec-k", "8", "--power", "2"]))
        );
        assert_eq!(a[7].name(), "f300-rind-fec8-p5-1");
    }

    #[test]
    fn a_failed_run_is_counted_the_matrix_goes_on_and_the_end_fails() {
        let d = Scratch::new("matrix");
        let c = crate::orch::Cfg {
            pi: "pi".into(),
            pi_dir: "d".into(),
            bin: "bin".into(),
            runs: d.0.clone(),
            port: Some("/dev/null".into()),
            frames: "f".into(),
            antenna: true,
        };
        let mut f = Fake::default();
        f.rule("--clock-check", 1, "", vec![]); // every run stops at its clock check
        let mut said = Vec::new();
        let steps = bench_plan(5, "L", "P");
        let k = Keys {
            lkey: "L".into(),
            pkey: "P".into(),
        };
        let e = run_matrix(&mut f, &c, false, &k, &steps, &mut |l| {
            said.push(l.to_string())
        })
        .unwrap_err();
        assert_eq!(e.to_string(), "BENCH_FAILED: 8 run(s) failed");
        assert_eq!(
            said.iter()
                .filter(|l| l.starts_with("FAILED: clock check failed"))
                .count(),
            8
        );
        assert!(!said.iter().any(|l| l == "BENCH_DONE"));
    }

    #[test]
    fn clock_lines_are_valid_json_or_absent() {
        let c = crate::orch::Cfg {
            pi: "pi".into(),
            pi_dir: "d".into(),
            bin: "bin".into(),
            runs: ".".into(),
            port: None,
            frames: "f".into(),
            antenna: false,
        };
        let mut f = Fake::default();
        f.rule(
            "--clock-check",
            0,
            "{\"remote_minus_local_ms\":1.00}\n",
            vec![],
        );
        assert_eq!(
            clock_line(&mut f, &c, 42).unwrap().unwrap(),
            "{\"t_us\":42,\"c\":{\"remote_minus_local_ms\":1.00}}"
        );
        f.rules[0].stdout = "ssh: no route".into();
        assert_eq!(clock_line(&mut f, &c, 42).unwrap(), None);
        f.rules[0].code = 255;
        f.rules[0].stdout = "{}".into();
        assert_eq!(clock_line(&mut f, &c, 42).unwrap(), None);
    }

    #[test]
    fn stamp_has_the_date_format() {
        let s = stamp_now();
        assert_eq!(s.len(), 19);
        assert_eq!((&s[4..5], &s[10..11], &s[13..14]), ("-", " ", ":"));
    }
}
