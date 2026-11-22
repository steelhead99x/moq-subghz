//! The external programs the tools shell out to (ssh, scp, ffmpeg, the project's own binaries),
//! as a plan that `--dry-run` can print exactly as it would run it, and the host that runs it.
//! Orchestration talks to a [`Host`], so the tests drive it with a scripted one: no process, no
//! sleep, no clock.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Where a command's standard error goes, when it is not left alone.
#[derive(Clone, Debug, PartialEq)]
pub enum Stderr {
    /// `2>&1`
    ToStdout,
    /// `2> file`
    File(PathBuf),
}

/// One command line: a program and its arguments, no shell involved when it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub prog: String,
    pub args: Vec<String>,
    /// `> file`: standard output goes to a file instead of being captured.
    pub stdout: Option<PathBuf>,
    pub stderr: Option<Stderr>,
    /// Touches no radio and no remote host, so `--dry-run` really runs it (the project's own `--dry-run`).
    pub local_only: bool,
}

impl Plan {
    pub fn new<S: Into<String>>(
        prog: impl Into<String>,
        args: impl IntoIterator<Item = S>,
    ) -> Plan {
        Plan {
            prog: prog.into(),
            args: args.into_iter().map(Into::into).collect(),
            stdout: None,
            stderr: None,
            local_only: false,
        }
    }

    /// Send standard output to `f`.
    pub fn out(mut self, f: impl Into<PathBuf>) -> Plan {
        self.stdout = Some(f.into());
        self
    }

    /// Send standard error to the same place as standard output.
    pub fn err_to_out(mut self) -> Plan {
        self.stderr = Some(Stderr::ToStdout);
        self
    }

    /// Send standard error to `f`.
    pub fn err(mut self, f: impl Into<PathBuf>) -> Plan {
        self.stderr = Some(Stderr::File(f.into()));
        self
    }

    /// Mark the plan as safe to run in a dry run.
    pub fn local(mut self) -> Plan {
        self.local_only = true;
        self
    }

    /// The line as a POSIX shell would need it, for `--dry-run` and error messages.
    pub fn shell(&self) -> String {
        let mut s = std::iter::once(self.prog.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(crate::factory::shq)
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(o) = &self.stdout {
            s += &format!(" > {}", crate::factory::shq(&o.display().to_string()));
        }
        match &self.stderr {
            Some(Stderr::ToStdout) => s += " 2>&1",
            Some(Stderr::File(f)) => {
                s += &format!(" 2> {}", crate::factory::shq(&f.display().to_string()))
            }
            None => {}
        }
        s
    }

    fn command(&self) -> Result<Command> {
        let mut c = Command::new(&self.prog);
        c.args(&self.args);
        if let Some(o) = &self.stdout {
            let f = File::create(o).with_context(|| format!("create {}", o.display()))?;
            if self.stderr == Some(Stderr::ToStdout) {
                c.stderr(f.try_clone()?);
            }
            c.stdout(f);
        }
        if let Some(Stderr::File(e)) = &self.stderr {
            c.stderr(File::create(e).with_context(|| format!("create {}", e.display()))?);
        }
        Ok(c)
    }
}

/// How a command ended: its exit code (128 + signal when killed) and the standard output it was
/// not redirected away from.
#[derive(Clone, Debug, PartialEq)]
pub struct Done {
    pub code: i32,
    pub stdout: String,
}

fn code(st: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

/// Where plans run: for real, printed only, or scripted in a test.
pub trait Host {
    /// Run to the end. An error means the program could not start; a failing exit is in the code.
    fn run(&mut self, p: &Plan) -> Result<Done>;
    /// Start in the background; `wait` joins it.
    fn start(&mut self, p: &Plan) -> Result<usize>;
    fn wait(&mut self, id: usize) -> Result<i32>;
    /// Stop a job `start` began and reap it; a job already joined is not an error.
    fn kill(&mut self, id: usize);
    fn sleep(&mut self, d: Duration);
    /// True when nothing really runs, so a caller skips checks of files that were never written.
    fn dry(&self) -> bool {
        false
    }
}

/// Runs the plans as processes.
#[derive(Default)]
pub struct Real {
    jobs: Vec<Option<Child>>,
}

impl Drop for Real {
    /// A job still running when the host goes away (an early return or a panic) is killed and
    /// reaped, so a capture never keeps its serial port.
    fn drop(&mut self) {
        for id in 0..self.jobs.len() {
            self.kill(id);
        }
    }
}

impl Host for Real {
    fn run(&mut self, p: &Plan) -> Result<Done> {
        let mut c = p.command()?;
        if p.stdout.is_some() {
            let st = c.status().with_context(|| format!("start {}", p.shell()))?;
            return Ok(Done {
                code: code(st),
                stdout: String::new(),
            });
        }
        let out = c.output().with_context(|| format!("start {}", p.shell()))?;
        if p.stderr.is_none() {
            // Not captured by the plan: show it, as the shell scripts did.
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(Done {
            code: code(out.status),
            stdout: String::from_utf8_lossy(&out.stdout).into(),
        })
    }

    fn start(&mut self, p: &Plan) -> Result<usize> {
        let mut c = p.command()?;
        c.stdin(Stdio::null());
        self.jobs.push(Some(
            c.spawn().with_context(|| format!("start {}", p.shell()))?,
        ));
        Ok(self.jobs.len() - 1)
    }

    fn wait(&mut self, id: usize) -> Result<i32> {
        let mut ch = self
            .jobs
            .get_mut(id)
            .and_then(Option::take)
            .context("no such job")?;
        Ok(code(ch.wait()?))
    }

    fn kill(&mut self, id: usize) {
        if let Some(mut ch) = self.jobs.get_mut(id).and_then(Option::take) {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Prints each plan instead of running it, except the ones that only compute.
pub struct Dry {
    real: Real,
}

impl Dry {
    pub fn new() -> Dry {
        Dry {
            real: Real::default(),
        }
    }
}

impl Host for Dry {
    fn run(&mut self, p: &Plan) -> Result<Done> {
        println!("{}", p.shell());
        if p.local_only {
            return self.real.run(p);
        }
        // The only output a caller reads from a remote command is the clock offset.
        Ok(Done {
            code: 0,
            stdout: "{\"remote_minus_local_ms\":0.00}\n".into(),
        })
    }

    fn start(&mut self, p: &Plan) -> Result<usize> {
        println!("{} &", p.shell());
        Ok(0)
    }

    fn wait(&mut self, _: usize) -> Result<i32> {
        Ok(0)
    }

    fn kill(&mut self, _: usize) {}

    fn sleep(&mut self, d: Duration) {
        println!("sleep {}", d.as_secs_f64());
    }

    fn dry(&self) -> bool {
        true
    }
}

/// Run to completion; a non-zero exit is an error naming the command.
pub fn run_ok(p: &Plan) -> Result<()> {
    let d = Real::default().run(p)?;
    if d.code != 0 {
        bail!("{} exited with {}", p.shell(), d.code);
    }
    Ok(())
}

/// Run to completion and return its standard output; a non-zero exit is an error.
pub fn capture(p: &Plan) -> Result<String> {
    let d = Real::default().run(p)?;
    if d.code != 0 {
        bail!("{} exited with {}", p.shell(), d.code);
    }
    Ok(d.stdout)
}

/// The last line of a file, as `tail -n 1` prints it (empty for an empty file).
pub fn last_line(text: &str) -> &str {
    text.lines().last().unwrap_or("")
}

/// Read a file the run was meant to leave behind, naming it when it is not there.
pub fn read(path: &Path) -> Result<String> {
    crate::cell::read_text(path, crate::cell::MAX_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quotes_what_needs_it() {
        let p = Plan::new("ssh", ["pi", "echo 'a b'", "x"]);
        assert_eq!(p.shell(), "ssh pi 'echo '\\''a b'\\''' x");
        let p = Plan::new("a", ["b"]).out("o.txt").err_to_out();
        assert_eq!(p.shell(), "a b > o.txt 2>&1");
        assert_eq!(
            Plan::new("a", Vec::<String>::new()).err("e e").shell(),
            "a 2> 'e e'"
        );
    }

    #[test]
    fn failure_is_an_error() {
        assert!(run_ok(&Plan::new("false", Vec::<String>::new())).is_err());
        assert!(run_ok(&Plan::new("true", Vec::<String>::new())).is_ok());
        assert!(capture(&Plan::new("sh", ["-c", "echo hi; exit 3"])).is_err());
        assert_eq!(capture(&Plan::new("echo", ["hi"])).unwrap(), "hi\n");
    }

    #[test]
    fn exit_codes_and_redirects() {
        let dir = crate::testutil::Scratch::new("exec");
        let f = dir.0.join("o");
        let p = Plan::new("sh", ["-c", "echo out; echo err >&2; exit 7"])
            .out(&f)
            .err_to_out();
        let mut h = Real::default();
        assert_eq!(h.run(&p).unwrap().code, 7);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "out\nerr\n");
        let id = h.start(&Plan::new("sh", ["-c", "exit 4"])).unwrap();
        assert_eq!(h.wait(id).unwrap(), 4);
        assert!(h.wait(id).is_err(), "a job is joined once");
        let id = h.start(&Plan::new("sleep", ["30"])).unwrap();
        h.kill(id);
        assert!(h.wait(id).is_err(), "a killed job is reaped");
        h.kill(id); // a second kill is harmless
        assert_eq!(last_line("a\nb\n"), "b");
        assert_eq!(last_line(""), "");
    }
}
