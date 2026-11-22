//! The external programs the tools shell out to (ssh, scp, ffmpeg, the project's own binaries),
//! as a plan that `--dry-run` can print exactly as it would run it.

use anyhow::{bail, Context, Result};
use std::process::Command;

/// One command line: a program and its arguments, no shell involved when it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub prog: String,
    pub args: Vec<String>,
}

impl Plan {
    pub fn new<S: Into<String>>(
        prog: impl Into<String>,
        args: impl IntoIterator<Item = S>,
    ) -> Plan {
        Plan {
            prog: prog.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// The line as a POSIX shell would need it, for `--dry-run` and error messages.
    pub fn shell(&self) -> String {
        std::iter::once(self.prog.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(crate::factory::shq)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.prog);
        c.args(&self.args);
        c
    }
}

/// Run to completion; a non-zero exit is an error naming the command.
pub fn run_ok(p: &Plan) -> Result<()> {
    let st = p
        .command()
        .status()
        .with_context(|| format!("start {}", p.shell()))?;
    if !st.success() {
        bail!("{} exited with {st}", p.shell());
    }
    Ok(())
}

/// Run to completion and return its standard output; a non-zero exit is an error.
pub fn capture(p: &Plan) -> Result<String> {
    let out = p
        .command()
        .output()
        .with_context(|| format!("start {}", p.shell()))?;
    if !out.status.success() {
        bail!(
            "{} exited with {}: {}",
            p.shell(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quotes_what_needs_it() {
        let p = Plan::new("ssh", ["pi", "echo 'a b'", "x"]);
        assert_eq!(p.shell(), "ssh pi 'echo '\\''a b'\\''' x");
    }

    #[test]
    fn failure_is_an_error() {
        assert!(run_ok(&Plan::new("false", Vec::<String>::new())).is_err());
        assert!(run_ok(&Plan::new("true", Vec::<String>::new())).is_ok());
        assert!(capture(&Plan::new("sh", ["-c", "echo hi; exit 3"])).is_err());
        assert_eq!(capture(&Plan::new("echo", ["hi"])).unwrap(), "hi\n");
    }
}
