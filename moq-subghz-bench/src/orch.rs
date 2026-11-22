//! `run`, `rev` and `lora2015-run`: one over-the-air run each, ports of `scripts/run2.sh`,
//! `scripts/rev.sh` and `scripts/lora2015-run.sh`. They start the project's own binaries and ssh,
//! then check every artifact: a run counts only when the sender, the receiver or logger and the
//! log copy all succeeded and left their summaries. Any failure is `FAILED <run>: <why>`, exit 1.
//!
//! They transmit, so each needs `--i-have-an-antenna` and `--transmit` (and a licence or permit
//! as per docs/LICENSING.md, which the sender checks). Neither flag is ever added for the user:
//! they are forwarded only when given here, or among the sender arguments after the profile. Nothing opens a serial port here: the project's
//! receiver and logger do, on the `--port` the user gives.

use crate::exec::{last_line, read, Host, Plan};
use crate::factory::shq;
use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where things are: the same settings `scripts/env.sh` held.
#[derive(Args, Clone, Debug)]
pub struct Env {
    /// ssh host of the Pi
    #[arg(long, env = "PI", default_value = "pi")]
    pub pi: String,
    /// Directory on the Pi (under its home) that holds moq-subghz-send and moq-subghz-recv
    #[arg(long, env = "PI_DIR", default_value = "moq-subghz")]
    pub pi_dir: String,
    /// Directory of the built binaries (default: the directory of this one)
    #[arg(long, env = "BIN")]
    pub bin: Option<PathBuf>,
    /// Where the runs are written
    #[arg(long, env = "RUNS", default_value = "runs")]
    pub runs: PathBuf,
    /// Serial device of the CatWAN stick. No default: it is opened by the receiver, logger or sender
    #[arg(long, env = "PORT")]
    pub port: Option<PathBuf>,
    /// Root of this repository, for the test pictures
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
    pub repo_root: PathBuf,
    /// Required to transmit: the antennas are on both radios. Forwarded to the sender only when given
    #[arg(long)]
    pub i_have_an_antenna: bool,
    /// Required to transmit (the licence gate): allow the sender to transmit. Forwarded only when given
    #[arg(long)]
    pub transmit: bool,
    /// Print the commands and stop (the only thing it runs is the sender's own --dry-run)
    #[arg(long)]
    pub dry_run: bool,
}

/// `Env` with its defaults filled in.
#[derive(Clone, Debug)]
pub struct Cfg {
    pub pi: String,
    pub pi_dir: String,
    pub bin: PathBuf,
    pub runs: PathBuf,
    pub port: Option<PathBuf>,
    pub frames: PathBuf,
    pub antenna: bool,
    pub transmit: bool,
}

impl Env {
    pub fn cfg(&self) -> Result<Cfg> {
        let bin = match &self.bin {
            Some(b) => b.clone(),
            None => std::env::current_exe()?
                .parent()
                .context("current exe has no directory")?
                .to_path_buf(),
        };
        Ok(Cfg {
            pi: self.pi.clone(),
            pi_dir: self.pi_dir.clone(),
            bin,
            runs: self.runs.clone(),
            port: self.port.clone(),
            frames: self
                .repo_root
                .canonicalize()
                .unwrap_or_else(|_| self.repo_root.clone())
                .join("results/lora-2015/frames"),
            antenna: self.i_have_an_antenna,
            transmit: self.transmit,
        })
    }
}

impl Cfg {
    pub fn bin(&self, name: &str) -> String {
        self.bin.join(name).display().to_string()
    }

    fn port(&self) -> Result<String> {
        Ok(self
            .port
            .as_ref()
            .context("--port (or PORT) is needed: no serial device is assumed")?
            .display()
            .to_string())
    }

    fn run_file(&self, name: &str, suffix: &str) -> PathBuf {
        self.runs.join(format!("{name}{suffix}"))
    }

    /// `ssh PI <script>`.
    fn ssh(&self, script: String) -> Plan {
        Plan::new("ssh", [self.pi.clone(), script])
    }

    /// A real run needs the user to have passed `--i-have-an-antenna` and `--transmit`, here or in
    /// the sender arguments. `--dry-run` needs neither (it transmits nothing).
    fn gate(&self, h: &dyn Host, args: &[String]) -> Result<()> {
        let given = |f: &str| args.iter().any(|a| a == f);
        ensure!(
            self.antenna || given("--i-have-an-antenna") || h.dry(),
            "refusing to transmit without --i-have-an-antenna"
        );
        ensure!(
            self.transmit || given("--transmit") || h.dry(),
            "refusing to transmit without --transmit (and a licence or permit, see docs/LICENSING.md)"
        );
        Ok(())
    }

    /// The transmit flags the user gave here and did not repeat in `args`, in that order. Never
    /// anything the user did not type.
    pub fn tx_flags(&self, args: &[String]) -> Vec<String> {
        let given = |f: &str| args.iter().any(|a| a == f);
        let mut v = Vec::new();
        if self.antenna && !given("--i-have-an-antenna") {
            v.push("--i-have-an-antenna".to_string());
        }
        if self.transmit && !given("--transmit") {
            v.push("--transmit".to_string());
        }
        v
    }

    fn prepare(&self, h: &dyn Host, name: &str) -> Result<()> {
        valid_name(name)?;
        if !h.dry() {
            fs::create_dir_all(&self.runs)
                .with_context(|| format!("create {}", self.runs.display()))?;
        }
        Ok(())
    }
}

/// A run name goes into file names and remote paths: keep it plain.
pub fn valid_name(n: &str) -> Result<()> {
    ensure!(
        !n.is_empty()
            && !n.starts_with(['-', '.'])
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
        "run name {n:?}: use letters, digits, '_', '-' and '.' only"
    );
    Ok(())
}

/// The Pi minus laptop clock offset from `moq-subghz-recv --clock-check`, as the text it printed
/// (kept as is, for the run's files) and as a number. Anything but a plain decimal is an error.
pub fn parse_offset(line: &str) -> Result<(String, f64)> {
    const KEY: &str = "remote_minus_local_ms\":";
    let bad = || format!("clock offset is not a number: {line:?}");
    let rest = &line[line.find(KEY).with_context(bad)? + KEY.len()..];
    let t = rest.split(['}', ',']).next().unwrap_or("").trim();
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let body = t.strip_prefix('-').unwrap_or(t);
    let ok = match body.split_once('.') {
        Some((i, f)) => digits(i) && digits(f),
        None => digits(body),
    };
    ensure!(ok, "{}", bad());
    Ok((t.to_string(), t.parse()?))
}

fn clock(h: &mut dyn Host, c: &Cfg) -> Result<(String, f64)> {
    let d = h.run(&Plan::new(
        c.bin("moq-subghz-recv"),
        ["--clock-check", &c.pi],
    ))?;
    ensure!(d.code == 0, "clock check failed");
    parse_offset(&d.stdout)
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

/// Save the last line of the sender's output as its summary, as `tail -1 > file` did.
fn save_summary(from: &Path, to: &Path) -> Result<()> {
    let text = fs::read_to_string(from).unwrap_or_default();
    let line = last_line(&text);
    write(
        to,
        &if line.is_empty() {
            String::new()
        } else {
            format!("{line}\n")
        },
    )
}

/// One run of a bench.
pub struct Run<'a> {
    pub name: &'a str,
    pub profile: &'a str,
    pub secs: u32,
    /// The receiver's key file (a path on the machine the receiver runs on)
    pub rx_key: Option<&'a str>,
    /// Other receiver arguments, split on spaces
    pub rx: &'a str,
    /// Play the video while it is received (needs ffplay)
    pub play: bool,
    /// Sender arguments
    pub args: &'a [String],
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

/// The remote command that runs the Pi's sender, keeping its output in a file so ssh exits with
/// the sender's own status.
pub fn pi_send_script(pi_dir: &str, r: &Run, flags: &[String]) -> String {
    let n = r.name;
    let so = shq(&format!("/tmp/{n}_send.out"));
    let args: String = flags
        .iter()
        .chain(r.args)
        .map(|a| format!("{} ", shq(a)))
        .collect();
    format!(
        "cd ~/{} && ./moq-subghz-send --profile {} --secs {} --log {} {args}> {so} 2>&1; st=$?; tail -n 1 {so}; rm -f {so}; exit $st",
        shq(pi_dir),
        shq(r.profile),
        r.secs,
        shq(&format!("/tmp/{n}_send.jsonl")),
    )
}

/// Pi to laptop: the receiver here, the sender on the Pi (`run2.sh`).
pub fn forward(h: &mut dyn Host, c: &Cfg, r: &Run) -> Result<()> {
    let n = r.name;
    c.gate(h, r.args)?;
    c.prepare(h, n)?;
    let (off, _) = clock(h, c)?;
    if !h.dry() {
        write(&c.run_file(n, ".clock_start"), &format!("{off}\n"))?;
    }
    let mut a = vec![
        "--port".to_string(),
        c.port()?,
        "--profile".into(),
        r.profile.into(),
        "--secs".into(),
        (r.secs + 10).to_string(),
    ];
    a.extend(["--out".into(), c.runs.join(n).display().to_string()]);
    if r.play {
        a.push("--play".into());
    }
    a.extend([
        "--decode".into(),
        "--raw-log".into(),
        format!("--clock-offset-ms={off}"),
    ]);
    if let Some(k) = r.rx_key {
        a.extend(["--key-file".into(), k.into()]);
    }
    a.extend(words(r.rx));
    let rx = h.start(
        &Plan::new(c.bin("moq-subghz-recv"), a)
            .out(c.run_file(n, ".out"))
            .err_to_out(),
    )?;
    h.sleep(Duration::from_secs(3));
    let send_st = h
        .run(
            &c.ssh(pi_send_script(&c.pi_dir, r, &c.tx_flags(r.args)))
                .out(c.run_file(n, ".send")),
        )?
        .code;
    let recv_st = h.wait(rx)?;
    if !h.dry() {
        fs::create_dir_all(c.runs.join(n))?;
    }
    let jsonl = format!("/tmp/{n}_send.jsonl");
    let log_st = h
        .run(
            &c.ssh(format!("cat {j} && rm -f {j}", j = shq(&jsonl)))
                .out(c.runs.join(n).join("send.jsonl")),
        )?
        .code;
    if h.dry() {
        h.run(
            &Plan::new(c.bin("moq-subghz-recv"), ["--clock-check", &c.pi])
                .out(c.run_file(n, ".clock_end")),
        )?;
        return Ok(());
    }
    save_summary(
        &c.run_file(n, ".send"),
        &c.runs.join(n).join("send_summary.json"),
    )?;
    h.run(
        &Plan::new(c.bin("moq-subghz-recv"), ["--clock-check", &c.pi])
            .out(c.run_file(n, ".clock_end")),
    )?;
    // Everything above is kept for a look; the run only counts when both ends succeeded.
    let fail = |why: String| -> Result<()> { bail!("FAILED {n}: {why}") };
    if send_st != 0 {
        return fail(format!(
            "sender exited {send_st} (see {})",
            c.run_file(n, ".send").display()
        ));
    }
    if recv_st != 0 {
        return fail(format!(
            "receiver exited {recv_st} (see {})",
            c.run_file(n, ".out").display()
        ));
    }
    let log = c.runs.join(n).join("send.jsonl");
    if log_st != 0 || fs::metadata(&log).map_or(true, |m| m.len() == 0) {
        return fail("no sender log".into());
    }
    if !read(&c.runs.join(n).join("send_summary.json"))?.contains("\"role\":\"send\"") {
        return fail(format!(
            "no sender summary in {}",
            c.run_file(n, ".send").display()
        ));
    }
    if !fs::read_to_string(c.runs.join(n).join("summary.json"))
        .unwrap_or_default()
        .contains("\"role\":\"recv\"")
    {
        return fail("no receiver summary.json".into());
    }
    println!("DONE {n}");
    Ok(())
}

/// Laptop to CatWAN to Pi: the receiver on the Pi, the sender here (`rev.sh`).
pub fn reverse(h: &mut dyn Host, c: &Cfg, r: &Run) -> Result<()> {
    let n = r.name;
    c.gate(h, r.args)?;
    c.prepare(h, n)?;
    let (x, xf) = clock(h, c)?;
    if !h.dry() {
        write(&c.run_file(n, ".clock_start"), &format!("{x}\n"))?;
    }
    // Capture clock (laptop) minus receive clock (Pi), printed like Python's repr of a float.
    let off = format!("{:?}", -xf);
    let tmp = shq(&format!("/tmp/{n}"));
    let mut rxa = format!(
        "./moq-subghz-recv --radio --profile {} --secs {} --out {tmp} --decode --raw-log --clock-offset-ms={}",
        shq(r.profile),
        r.secs + 10,
        shq(&off)
    );
    if let Some(k) = r.rx_key {
        rxa += &format!(" --key-file {}", shq(k));
    }
    for w in words(r.rx) {
        rxa += &format!(" {}", shq(&w));
    }
    let script = format!("cd ~/{} && rm -rf {tmp} && {rxa}", shq(&c.pi_dir));
    let rx = h.start(&c.ssh(script).out(c.run_file(n, ".out")).err_to_out())?;
    h.sleep(Duration::from_secs(4));
    let mut a = vec![
        "--catwan".to_string(),
        c.port()?,
        "--profile".into(),
        r.profile.into(),
        "--secs".into(),
        r.secs.to_string(),
    ];
    a.extend([
        "--log".into(),
        c.run_file(n, ".send.jsonl").display().to_string(),
    ]);
    a.extend(c.tx_flags(r.args));
    a.extend(r.args.iter().cloned());
    let send = Plan::new(c.bin("moq-subghz-send"), a)
        .out(c.run_file(n, ".send"))
        .err(c.run_file(n, ".send.err"));
    let send_st = h.run(&send)?.code;
    let recv_st = h.wait(rx)?; // ssh exits with the remote receiver's status
    if !h.dry() {
        fs::create_dir_all(c.runs.join(n))?;
    }
    // scp's glob is expanded on the Pi; the Pi's copy is removed only after a good copy.
    let mut copy_st = h
        .run(&Plan::new(
            "scp",
            [
                "-q".to_string(),
                format!("{}:/tmp/{n}/*", c.pi),
                format!("{}/", c.runs.join(n).display()),
            ],
        ))?
        .code;
    if copy_st == 0 {
        copy_st = h.run(&c.ssh(format!("rm -rf {tmp}")))?.code;
    }
    if h.dry() {
        h.run(
            &Plan::new(c.bin("moq-subghz-recv"), ["--clock-check", &c.pi])
                .out(c.run_file(n, ".clock_end")),
        )?;
        return Ok(());
    }
    let (sj, dj) = (
        c.run_file(n, ".send.jsonl"),
        c.runs.join(n).join("send.jsonl"),
    );
    if sj.exists() {
        fs::rename(&sj, &dj).with_context(|| format!("move {}", sj.display()))?;
    }
    save_summary(
        &c.run_file(n, ".send"),
        &c.runs.join(n).join("send_summary.json"),
    )?;
    h.run(
        &Plan::new(c.bin("moq-subghz-recv"), ["--clock-check", &c.pi])
            .out(c.run_file(n, ".clock_end")),
    )?;
    let fail = |why: String| -> Result<()> { bail!("FAILED {n}: {why}") };
    if send_st != 0 {
        return fail(format!(
            "sender exited {send_st} (see {})",
            c.run_file(n, ".send.err").display()
        ));
    }
    if recv_st != 0 {
        return fail(format!(
            "receiver exited {recv_st} (see {})",
            c.run_file(n, ".out").display()
        ));
    }
    if copy_st != 0 {
        return fail("copying the receiver's files from the Pi failed".into());
    }
    if fs::metadata(&dj).map_or(true, |m| m.len() == 0) {
        return fail("no sender log".into());
    }
    if !read(&c.runs.join(n).join("send_summary.json"))?.contains("\"role\":\"send\"") {
        return fail("no sender summary".into());
    }
    if !fs::read_to_string(c.runs.join(n).join("summary.json"))
        .unwrap_or_default()
        .contains("\"role\":\"recv\"")
    {
        return fail("no receiver summary.json".into());
    }
    println!("DONE {n}");
    Ok(())
}

/// One lora2015 picture run.
pub struct Lora<'a> {
    pub name: &'a str,
    pub mode: &'a str,
    /// A file name under `results/lora-2015/frames/`
    pub file: &'a str,
    pub profile: &'a str,
    pub passes: u32,
    pub args: &'a [String],
}

/// Seconds the logger has to listen: `passes` passes of `pass_s` plus a second each, and 14 more.
pub fn log_secs(pass_s: f64, passes: u32) -> i64 {
    (passes as f64 * (pass_s + 1.0) + 14.0) as i64
}

fn after_digits(text: &str, key: &str, dot: bool) -> Option<String> {
    text.lines().find_map(|l| {
        let i = l.find(key)? + key.len();
        let s: String = l[i..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || (dot && *c == '.'))
            .collect();
        (!s.is_empty()).then_some(s)
    })
}

/// Logger here, sender on the Pi, then the exact loss from a replay (`lora2015-run.sh`).
pub fn lora(h: &mut dyn Host, c: &Cfg, r: &Lora) -> Result<()> {
    let n = r.name;
    c.gate(h, r.args)?;
    c.prepare(h, n)?;
    valid_name(r.file).context("picture file name")?;
    let frame = c.frames.join(r.file);
    let dry = Plan::new(
        c.bin("lora2015-send"),
        [
            "--mode",
            r.mode,
            "--file",
            &frame.display().to_string(),
            "--profile",
            r.profile,
            "--dry-run",
        ]
        .into_iter()
        .map(String::from)
        .chain(r.args.iter().cloned()),
    )
    .local();
    let d = h.run(&dry)?;
    ensure!(d.code == 0, "{} exited with {}", dry.shell(), d.code);
    let pass_s: f64 = after_digits(&d.stdout, "pass_s=", true)
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("no pass_s= in the output of {}", dry.shell()))?;
    let secs = log_secs(pass_s, r.passes);
    let reference: Vec<String> = if r.file.ends_with(".jpg") {
        vec!["--reference".into(), frame.display().to_string()]
    } else {
        vec![]
    };
    let rdir = c.runs.join(n);
    if !h.dry() {
        fs::create_dir_all(&rdir)?;
    }
    let mut a: Vec<String> = [
        "--port".to_string(),
        c.port()?,
        "--mode".into(),
        r.mode.into(),
        "--profile".into(),
        r.profile.into(),
        "--secs".into(),
        secs.to_string(),
    ]
    .into();
    a.extend(["--out".into(), rdir.display().to_string()]);
    a.extend(reference.iter().cloned());
    let lg = h.start(
        &Plan::new(c.bin("lora2015-log"), a)
            .out(c.run_file(n, ".log.out"))
            .err_to_out(),
    )?;
    h.sleep(Duration::from_secs(3));
    let so = shq(&format!("/tmp/{n}.send.out"));
    let args: String = c
        .tx_flags(r.args)
        .iter()
        .chain(r.args)
        .map(|a| format!("{} ", shq(a)))
        .collect();
    let script = format!(
        "cd ~/lora2015 && ./lora2015-send --mode {} --file frames/{} --profile {} --power 5 --passes {} --tx-log {} {args}> {so} 2>&1; st=$?; tail -n 3 {so}; rm -f {so}; exit $st",
        shq(r.mode),
        shq(r.file),
        shq(r.profile),
        r.passes,
        shq(&format!("/tmp/{n}.tx.csv")),
    );
    let send_st = h.run(&c.ssh(script).out(c.run_file(n, ".send")))?.code;
    let log_st = h.wait(lg)?;
    let tx = format!("/tmp/{n}.tx.csv");
    let tx_st = h
        .run(&Plan::new(
            "scp",
            [
                "-q".to_string(),
                format!("{}:{tx}", c.pi),
                c.run_file(n, ".tx.csv").display().to_string(),
            ],
        ))?
        .code;
    h.run(&c.ssh(format!("rm -f {}", shq(&tx))))?;
    let sent = after_digits(
        &fs::read_to_string(c.run_file(n, ".send")).unwrap_or_default(),
        "sent frames=",
        false,
    );
    let mut exact_st = 1;
    if let Some(s) = &sent {
        let mut a: Vec<String> = [
            "--replay".to_string(),
            rdir.join("raw.log").display().to_string(),
            "--mode".into(),
            r.mode.into(),
            "--profile".into(),
            r.profile.into(),
            "--sent-frames".into(),
            s.clone(),
            "--out".into(),
            rdir.join("exact").display().to_string(),
        ]
        .into();
        a.extend(reference);
        exact_st = h
            .run(
                &Plan::new(c.bin("lora2015-log"), a)
                    .out(c.run_file(n, ".exact.out"))
                    .err_to_out(),
            )?
            .code;
    } else if h.dry() {
        return Ok(());
    }
    if h.dry() {
        return Ok(());
    }
    let fail = |why: String| -> Result<()> { bail!("FAILED {n}: {why}") };
    if send_st != 0 {
        return fail(format!(
            "sender exited {send_st} (see {})",
            c.run_file(n, ".send").display()
        ));
    }
    if log_st != 0 {
        return fail(format!(
            "logger exited {log_st} (see {})",
            c.run_file(n, ".log.out").display()
        ));
    }
    let Some(sent) = sent else {
        return fail(format!(
            "no sent-frame count in {}",
            c.run_file(n, ".send").display()
        ));
    };
    if tx_st != 0 || fs::metadata(c.run_file(n, ".tx.csv")).map_or(true, |m| m.len() == 0) {
        return fail("no per frame log (tx.csv) copied from the Pi".into());
    }
    if exact_st != 0 || fs::metadata(rdir.join("exact/images.csv")).map_or(true, |m| m.len() == 0) {
        return fail(format!(
            "exact replay failed (see {})",
            c.run_file(n, ".exact.out").display()
        ));
    }
    let line = read(&c.run_file(n, ".send"))?
        .lines()
        .find(|l| l.contains("sent frames"))
        .unwrap_or("")
        .to_string();
    println!("DONE {n} sent={sent} {line}");
    Ok(())
}

#[cfg(test)]
pub mod fake {
    //! A scripted host for the tests: records every plan, answers from rules, writes the files a
    //! rule names, and never sleeps.
    use super::*;
    use crate::exec::Done;

    pub struct Rule {
        pub pat: &'static str,
        pub code: i32,
        pub stdout: String,
        pub files: Vec<(PathBuf, String)>,
    }

    #[derive(Default)]
    pub struct Fake {
        pub rules: Vec<Rule>,
        pub log: Vec<String>,
        pub slept: Vec<Duration>,
        bg: Vec<i32>,
    }

    impl Fake {
        pub fn rule(
            &mut self,
            pat: &'static str,
            code: i32,
            stdout: &str,
            files: Vec<(PathBuf, &str)>,
        ) {
            self.rules.push(Rule {
                pat,
                code,
                stdout: stdout.into(),
                files: files.into_iter().map(|(p, t)| (p, t.to_string())).collect(),
            });
        }

        fn apply(&mut self, p: &Plan) -> Done {
            self.log.push(p.shell());
            let line = p.shell();
            let (code, stdout) = match self.rules.iter().find(|r| line.contains(r.pat)) {
                Some(r) => {
                    for (f, t) in &r.files {
                        fs::create_dir_all(f.parent().unwrap()).unwrap();
                        fs::write(f, t).unwrap();
                    }
                    (r.code, r.stdout.clone())
                }
                None => (0, String::new()),
            };
            match &p.stdout {
                Some(f) => {
                    fs::create_dir_all(f.parent().unwrap()).unwrap();
                    fs::write(f, &stdout).unwrap();
                    Done {
                        code,
                        stdout: String::new(),
                    }
                }
                None => Done { code, stdout },
            }
        }
    }

    impl Host for Fake {
        fn run(&mut self, p: &Plan) -> Result<Done> {
            Ok(self.apply(p))
        }
        fn start(&mut self, p: &Plan) -> Result<usize> {
            let d = self.apply(p);
            self.bg.push(d.code);
            Ok(self.bg.len() - 1)
        }
        fn wait(&mut self, id: usize) -> Result<i32> {
            Ok(self.bg[id])
        }
        fn sleep(&mut self, d: Duration) {
            self.slept.push(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;
    use crate::testutil::Scratch;

    fn cfg(dir: &Path) -> Cfg {
        Cfg {
            pi: "pi".into(),
            pi_dir: "moq-subghz".into(),
            bin: "bin".into(),
            runs: dir.to_path_buf(),
            port: Some("/dev/ttyTEST".into()),
            frames: "frames".into(),
            antenna: true,
            transmit: true,
        }
    }

    const CLOCK: &str = "{\"host\":\"pi\",\"best_rtt_ms\":1.00,\"remote_minus_local_ms\":-12.50}\n";

    fn run<'a>(name: &'a str, args: &'a [String]) -> Run<'a> {
        Run {
            name,
            profile: "300000:100000:6",
            secs: 330,
            rx_key: Some("my key.hex"),
            rx: "--x 1",
            play: false,
            args,
        }
    }

    /// A forward run whose ends all succeed leaves the artifacts the checks look for.
    fn good_forward(f: &mut Fake, d: &Path) {
        f.rule("--clock-check", 0, CLOCK, vec![]);
        f.rule(
            "moq-subghz-recv --port",
            0,
            "",
            vec![(d.join("t/summary.json"), "{\"role\":\"recv\"}")],
        );
        f.rule(
            "moq-subghz-send --profile",
            0,
            "{\"role\":\"send\",\"fec_k\":8}\n",
            vec![],
        );
        f.rule("cat /tmp/t_send.jsonl", 0, "{\"seq\":1}\n", vec![]);
    }

    #[test]
    fn forward_plan_is_the_run2_sh_command_sequence() {
        let d = Scratch::new("fwd");
        let mut f = Fake::default();
        good_forward(&mut f, &d.0);
        let args = vec!["--fec-k".to_string(), "8".into()];
        forward(&mut f, &cfg(&d.0), &run("t", &args)).unwrap();
        let r = d.0.display();
        assert_eq!(f.log[0], "bin/moq-subghz-recv --clock-check pi");
        assert_eq!(
            f.log[1],
            format!("bin/moq-subghz-recv --port /dev/ttyTEST --profile 300000:100000:6 --secs 340 --out {r}/t --decode --raw-log --clock-offset-ms=-12.50 --key-file 'my key.hex' --x 1 > {r}/t.out 2>&1")
        );
        assert_eq!(
            f.log[2],
            format!("ssh pi 'cd ~/moq-subghz && ./moq-subghz-send --profile 300000:100000:6 --secs 330 --log /tmp/t_send.jsonl --i-have-an-antenna --transmit --fec-k 8 > /tmp/t_send.out 2>&1; st=$?; tail -n 1 /tmp/t_send.out; rm -f /tmp/t_send.out; exit $st' > {r}/t.send")
        );
        assert_eq!(f.slept, [Duration::from_secs(3)]);
        assert_eq!(
            fs::read_to_string(d.0.join("t.clock_start")).unwrap(),
            "-12.50\n"
        );
        assert_eq!(
            fs::read_to_string(d.0.join("t/send_summary.json")).unwrap(),
            "{\"role\":\"send\",\"fec_k\":8}\n"
        );
    }

    #[test]
    fn forward_fails_loud_on_each_missing_piece() {
        for (what, bad) in [
            ("sender exited 3", "sender"),
            ("receiver exited 2", "recv"),
            ("no sender log", "log"),
            ("no sender summary", "summary"),
            ("no receiver summary.json", "rsum"),
        ] {
            let d = Scratch::new("fwdfail");
            let mut f = Fake::default();
            f.rule("--clock-check", 0, CLOCK, vec![]);
            let rsum = if bad == "rsum" {
                "{}"
            } else {
                "{\"role\":\"recv\"}"
            };
            f.rule(
                "moq-subghz-recv --port",
                if bad == "recv" { 2 } else { 0 },
                "",
                vec![(d.0.join("t/summary.json"), rsum)],
            );
            let sum = if bad == "summary" {
                "garbage\n"
            } else {
                "{\"role\":\"send\"}\n"
            };
            f.rule(
                "moq-subghz-send --profile",
                if bad == "sender" { 3 } else { 0 },
                sum,
                vec![],
            );
            f.rule(
                "cat /tmp/t_send.jsonl",
                if bad == "log" { 1 } else { 0 },
                "{\"seq\":1}\n",
                vec![],
            );
            let e = forward(&mut f, &cfg(&d.0), &run("t", &[]))
                .unwrap_err()
                .to_string();
            assert!(
                e.starts_with("FAILED t: ") && e.contains(what),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn an_empty_sender_log_fails_the_run() {
        let d = Scratch::new("emptylog");
        let mut f = Fake::default();
        good_forward(&mut f, &d.0);
        f.rules.retain(|r| r.pat != "cat /tmp/t_send.jsonl");
        f.rule("cat /tmp/t_send.jsonl", 0, "", vec![]);
        assert!(forward(&mut f, &cfg(&d.0), &run("t", &[]))
            .unwrap_err()
            .to_string()
            .contains("no sender log"));
    }

    #[test]
    fn bad_clock_offset_stops_before_anything_transmits() {
        let d = Scratch::new("badclock");
        for out in [
            "{\"remote_minus_local_ms\":\"x\"}",
            "not json",
            "{}",
            "{\"remote_minus_local_ms\":1e3}",
            "{\"remote_minus_local_ms\":.5}",
            "{\"remote_minus_local_ms\":}",
        ] {
            let mut f = Fake::default();
            f.rule("--clock-check", 0, out, vec![]);
            let e = forward(&mut f, &cfg(&d.0), &run("t", &[]))
                .unwrap_err()
                .to_string();
            assert!(e.contains("clock offset is not a number"), "{e}");
            assert_eq!(f.log.len(), 1, "nothing but the clock check ran");
        }
        let mut f = Fake::default();
        f.rule("--clock-check", 1, "", vec![]);
        assert_eq!(
            forward(&mut f, &cfg(&d.0), &run("t", &[]))
                .unwrap_err()
                .to_string(),
            "clock check failed"
        );
    }

    #[test]
    fn nothing_transmits_without_the_antenna_flag_or_a_plain_name() {
        let d = Scratch::new("gate");
        let mut f = Fake::default();
        let mut c = cfg(&d.0);
        c.antenna = false;
        assert!(forward(&mut f, &c, &run("t", &[]))
            .unwrap_err()
            .to_string()
            .contains("--i-have-an-antenna"));
        assert!(reverse(&mut f, &c, &run("t", &[])).is_err());
        let l = Lora {
            name: "t",
            mode: "chunk",
            file: "a.jpg",
            profile: "sf7-125",
            passes: 1,
            args: &[],
        };
        assert!(lora(&mut f, &c, &l).is_err());
        assert!(f.log.is_empty());
        for bad in ["", "../x", "-x", "a b", "a;b", ".h"] {
            assert!(
                forward(&mut f, &cfg(&d.0), &run(bad, &[])).is_err(),
                "{bad:?}"
            );
        }
        assert!(f.log.is_empty());
    }

    #[test]
    fn nothing_transmits_without_the_transmit_flag() {
        let d = Scratch::new("gate-tx");
        let mut f = Fake::default();
        let mut c = cfg(&d.0);
        c.transmit = false;
        for e in [
            forward(&mut f, &c, &run("t", &[])).unwrap_err(),
            reverse(&mut f, &c, &run("t", &[])).unwrap_err(),
        ] {
            assert!(e.to_string().contains("without --transmit"), "{e}");
        }
        let l = Lora {
            name: "t",
            mode: "chunk",
            file: "a.jpg",
            profile: "sf7-125",
            passes: 1,
            args: &[],
        };
        assert!(lora(&mut f, &c, &l).is_err());
        assert!(f.log.is_empty());
        // Given among the sender arguments instead, it passes the gate (once, not doubled).
        let a = vec!["--transmit".to_string()];
        assert!(c.gate(&f, &a).is_ok());
        assert_eq!(c.tx_flags(&a), ["--i-have-an-antenna"]);
    }

    #[test]
    fn transmit_flags_are_forwarded_only_when_the_user_gave_them() {
        let d = Scratch::new("flags");
        let mut c = cfg(&d.0);
        assert_eq!(c.tx_flags(&[]), ["--i-have-an-antenna", "--transmit"]);
        let both = vec!["--i-have-an-antenna".to_string(), "--transmit".into()];
        assert!(c.tx_flags(&both).is_empty());
        c.antenna = false;
        c.transmit = false;
        assert!(c.tx_flags(&[]).is_empty());
        // A dry run with neither flag shows neither flag in any command.
        let mut h = crate::exec::Dry::new();
        forward(&mut h, &c, &run("t", &[])).unwrap();
        reverse(&mut h, &c, &run("t", &[])).unwrap();
        assert!(!pi_send_script("d", &run("t", &[]), &c.tx_flags(&[])).contains("--transmit"));
        assert!(!pi_send_script("d", &run("t", &[]), &c.tx_flags(&[])).contains("antenna"));
    }

    #[test]
    fn no_port_is_assumed() {
        let d = Scratch::new("noport");
        let mut f = Fake::default();
        f.rule("--clock-check", 0, CLOCK, vec![]);
        let mut c = cfg(&d.0);
        c.port = None;
        assert!(forward(&mut f, &c, &run("t", &[]))
            .unwrap_err()
            .to_string()
            .contains("--port"));
    }

    #[test]
    fn reverse_negates_the_offset_and_copies_before_it_removes() {
        let d = Scratch::new("rev");
        let mut f = Fake::default();
        f.rule("--clock-check", 0, CLOCK, vec![]);
        f.rule("moq-subghz-recv --radio", 0, "", vec![]);
        f.rule(
            "moq-subghz-send --catwan",
            0,
            "{\"role\":\"send\"}\n",
            vec![(d.0.join("t.send.jsonl"), "{\"seq\":1}\n")],
        );
        f.rule(
            "scp -q",
            0,
            "",
            vec![(d.0.join("t/summary.json"), "{\"role\":\"recv\"}")],
        );
        reverse(
            &mut f,
            &cfg(&d.0),
            &run("t", &["--fec-k".to_string(), "8".into()]),
        )
        .unwrap();
        assert!(
            f.log[1].contains("--clock-offset-ms=12.5 --key-file '\\''my key.hex'\\'' --x 1"),
            "{}",
            f.log[1]
        );
        assert_eq!(f.slept, [Duration::from_secs(4)]);
        assert!(f.log[2].starts_with(
            "bin/moq-subghz-send --catwan /dev/ttyTEST --profile 300000:100000:6 --secs 330 --log"
        ));
        assert!(f.log[2].contains("--i-have-an-antenna --transmit --fec-k 8 > "));
        let (scp, rm) = (
            f.log.iter().position(|l| l.starts_with("scp")).unwrap(),
            f.log
                .iter()
                .position(|l| l == "ssh pi 'rm -rf /tmp/t'")
                .unwrap(),
        );
        assert!(scp < rm);
        assert!(d.0.join("t/send.jsonl").exists() && !d.0.join("t.send.jsonl").exists());
    }

    #[test]
    fn a_failed_copy_keeps_the_pi_files_and_fails_the_run() {
        let d = Scratch::new("revcopy");
        let mut f = Fake::default();
        f.rule("--clock-check", 0, CLOCK, vec![]);
        f.rule(
            "moq-subghz-send --catwan",
            0,
            "{\"role\":\"send\"}\n",
            vec![(d.0.join("t.send.jsonl"), "x\n")],
        );
        f.rule("scp -q", 1, "", vec![]);
        let e = reverse(&mut f, &cfg(&d.0), &run("t", &[]))
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "FAILED t: copying the receiver's files from the Pi failed"
        );
        assert!(!f.log.iter().any(|l| l == "ssh pi 'rm -rf /tmp/t'"));
    }

    fn lora_job(d: &Path) -> Fake {
        let mut f = Fake::default();
        f.rule(
            "--dry-run",
            0,
            "summary mode=Chunk frames=35 air_bytes=8688 toa_s=13.638 pass_s=14.000\n",
            vec![],
        );
        f.rule(
            "lora2015-log --port",
            0,
            "",
            vec![(d.join("t/raw.log"), "raw")],
        );
        f.rule(
            "lora2015-send --mode",
            0,
            "sent frames=105 bytes=26064 wall_s=43.017\n",
            vec![],
        );
        f.rule("scp -q", 0, "", vec![(d.join("t.tx.csv"), "a,b\n1,2\n")]);
        f.rule(
            "--replay",
            0,
            "",
            vec![(d.join("t/exact/images.csv"), "image\n0\n")],
        );
        f
    }

    #[test]
    fn lora_run_listens_for_the_passes_and_replays_with_the_sent_count() {
        let d = Scratch::new("lora");
        let mut f = lora_job(&d.0);
        let l = Lora {
            name: "t",
            mode: "chunk",
            file: "f.jpg",
            profile: "sf7-125",
            passes: 3,
            args: &["--dr".to_string(), "dr3".into()],
        };
        lora(&mut f, &cfg(&d.0), &l).unwrap();
        assert!(
            f.log[1].contains("--secs 59 ") && f.log[1].contains("--reference frames/f.jpg"),
            "{}",
            f.log[1]
        );
        assert!(f
            .log
            .iter()
            .any(|l| l.contains("--sent-frames 105") && l.contains("--reference frames/f.jpg")));
        assert!(f.log.iter().any(|l| l.contains(
            "--power 5 --passes 3 --tx-log /tmp/t.tx.csv --i-have-an-antenna --transmit --dr dr3 >"
        )));
        assert_eq!(log_secs(14.0, 3), 59);
    }

    #[test]
    fn lora_run_fails_loud() {
        for (why, rule) in [
            ("sender exited 5", "send"),
            ("logger exited 6", "log"),
            ("no sent-frame count", "count"),
            ("exact replay failed", "replay"),
            ("tx.csv", "tx"),
        ] {
            let d = Scratch::new("lorafail");
            let mut f = lora_job(&d.0);
            match rule {
                "send" => f.rules[2].code = 5,
                "log" => f.rules[1].code = 6,
                "count" => f.rules[2].stdout = "no frames line\n".into(),
                "replay" => f.rules[4].code = 1,
                _ => f.rules[3].code = 1,
            }
            if rule == "tx" {
                f.rules[3].files.clear();
            }
            let l = Lora {
                name: "t",
                mode: "ssdv",
                file: "f.ssdv",
                profile: "sf7-125",
                passes: 1,
                args: &[],
            };
            let e = lora(&mut f, &cfg(&d.0), &l).unwrap_err().to_string();
            assert!(
                e.starts_with("FAILED t: ") && e.contains(why),
                "{rule}: {e}"
            );
        }
    }

    #[test]
    fn dry_run_prints_and_checks_nothing() {
        let d = Scratch::new("dry");
        let mut h = crate::exec::Dry::new();
        let mut c = cfg(&d.0);
        c.antenna = false;
        forward(&mut h, &c, &run("t", &[])).unwrap();
        reverse(&mut h, &c, &run("t", &[])).unwrap();
        assert!(!d.0.join("t").exists());
    }

    #[test]
    fn offset_text() {
        assert_eq!(parse_offset(CLOCK).unwrap(), ("-12.50".into(), -12.5));
        assert_eq!(
            parse_offset("{\"remote_minus_local_ms\":3}").unwrap().1,
            3.0
        );
    }
}
