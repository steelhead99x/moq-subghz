//! Bench and analysis tools for moq-subghz: the Rust replacement for the `scripts/` harness.

mod analyze;
mod cell;
mod demo;
mod exec;
mod factory;
mod lora2015;
mod matrix;
mod orch;
mod package;
mod serial;
mod stickcap;
mod uf2;

use anyhow::{Context, Result};
use clap::Args;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the CSVs and charts of results/stick-0.5.1 from the raw runs (scripts/analyze-051.py)
    Analyze {
        /// Directory of the raw runs
        runs: PathBuf,
        /// Where the CSVs and SVGs go
        out: PathBuf,
        /// Root of this repository, for the reference pictures and the factory results
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// Factory-vs-factory picture test: the stock Dragino app on the Pi, the stock receiver on the stick
    FactoryBench(factory::Opts),
    /// Raw logger for the stock receiver sketch on the stick (opens only --dev)
    StickCap(stickcap::Opts),
    /// The Pi side of factory-bench: send each chunk with the stock Dragino app
    FactorySend {
        /// Text file with one chunk per line
        chunks: PathBuf,
        /// Directory of the stock `dragino_lora_app`
        #[arg(long, default_value = ".")]
        app_dir: PathBuf,
        /// Required: the antenna is on the HAT
        #[arg(long)]
        i_have_an_antenna: bool,
        /// How long each send may run before it is cut off, in milliseconds
        #[arg(long, default_value_t = 1300)]
        timeout_ms: u64,
    },
    /// One Pi-to-laptop run: receiver here, sender on the Pi (scripts/run2.sh)
    Run(RunArgs),
    /// One laptop-to-Pi run: receiver on the Pi, sender here (scripts/rev.sh)
    Rev(RunArgs),
    /// The matched rind off/on matrix of the README, 8 runs (scripts/bench.sh)
    Bench {
        #[command(flatten)]
        env: orch::Env,
        #[command(flatten)]
        keys: KeyArgs,
        /// Seconds per run
        #[arg(long, env = "S", default_value_t = 330)]
        secs: u32,
        #[arg(long)]
        no_play: bool,
    },
    /// The whole results/stick-0.5.1 matrix (scripts/bench-051.sh)
    #[command(name = "bench-051")]
    Bench051 {
        #[command(flatten)]
        env: orch::Env,
        #[command(flatten)]
        keys: KeyArgs,
        #[arg(long, env = "PART", value_enum, default_value = "all")]
        part: matrix::Part,
        /// Repeats of each picture run
        #[arg(long, env = "REPS", default_value_t = 3)]
        reps: u32,
        /// Seconds per FSK run
        #[arg(long, env = "FSK_SECS", default_value_t = 120)]
        fsk_secs: u32,
        #[arg(long)]
        no_play: bool,
    },
    /// One lora2015 picture run: logger here, sender on the Pi, exact loss from a replay (scripts/lora2015-run.sh)
    Lora2015Run {
        #[command(flatten)]
        env: orch::Env,
        name: String,
        /// chunk, ssdv or lorawan
        mode: String,
        /// A file name under results/lora-2015/frames/
        file: String,
        profile: String,
        passes: u32,
        /// Sender arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The 6 minute guided demo of the stick (scripts/demo-051.sh)
    #[command(name = "demo-051")]
    Demo051 {
        #[command(flatten)]
        env: orch::Env,
        /// Where the demo is written (default RUNS/demo)
        out: Option<PathBuf>,
        /// Play the live video with ffplay
        #[arg(long)]
        play: bool,
    },
    /// Log the Pi-minus-laptop clock offset every 5 s until RUNS/clocklog.stop exists (scripts/clocklog.sh)
    Clocklog {
        #[command(flatten)]
        env: orch::Env,
    },
    /// Copy the data worth keeping from a bench-051 runs directory (scripts/package-051.sh)
    #[command(name = "package-051")]
    Package051 {
        runs: PathBuf,
        demo: PathBuf,
        out: PathBuf,
    },
    /// Print the calculated airtime table of results/lora-2015/README.md (scripts/lora2015-table.sh)
    Lora2015Table {
        /// Directory of the built binaries (default: the directory of this one)
        #[arg(long, env = "BIN")]
        bin: Option<PathBuf>,
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// Rebuild the test pictures of results/lora-2015/frames (scripts/lora2015-frames.sh)
    Lora2015Frames {
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// ELF to UF2 for the RP2040 (firmware/catwan-fw/elf2uf2.py)
    Elf2uf2 { elf: PathBuf, uf2: PathBuf },
}

/// Arguments of `run` and `rev`: one run, then the sender's own arguments.
#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    env: orch::Env,
    /// The receiver's key file (a path on the machine the receiver runs on)
    #[arg(long, env = "RX_KEY")]
    rx_key: Option<String>,
    /// Other receiver arguments, split on spaces
    #[arg(long, env = "RX", default_value = "", allow_hyphen_values = true)]
    rx: String,
    /// Do not play the video while it is received (ffplay is not installed)
    #[arg(long)]
    no_play: bool,
    name: String,
    /// moq-subghz profile, e.g. 300000:100000:6
    profile: String,
    secs: u32,
    /// Sender arguments, passed on as they are
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// Where the key files of the encrypted runs are.
#[derive(Args)]
struct KeyArgs {
    /// Key file on this machine
    #[arg(long, env = "LKEY")]
    lkey: Option<String>,
    /// Path of the same key on the Pi
    #[arg(long, env = "PKEY")]
    pkey: Option<String>,
}

impl KeyArgs {
    fn get(&self, pkey_default: &str) -> matrix::Keys {
        let home = std::env::var("HOME").unwrap_or_default();
        matrix::Keys {
            lkey: self
                .lkey
                .clone()
                .unwrap_or(format!("{home}/moq-subghz-key.hex")),
            pkey: self.pkey.clone().unwrap_or(pkey_default.into()),
        }
    }
}

fn host(dry: bool) -> Box<dyn exec::Host> {
    if dry {
        Box::new(exec::Dry::new())
    } else {
        Box::new(exec::Real::default())
    }
}

fn one_run(a: RunArgs, fwd: bool) -> Result<()> {
    let (c, mut h) = (a.env.cfg()?, host(a.env.dry_run));
    let r = orch::Run {
        name: &a.name,
        profile: &a.profile,
        secs: a.secs,
        rx_key: a.rx_key.as_deref(),
        rx: &a.rx,
        play: !a.no_play && fwd,
        args: &a.args,
    };
    if fwd {
        orch::forward(&mut *h, &c, &r)
    } else {
        orch::reverse(&mut *h, &c, &r)
    }
}

fn main() {
    if let Err(e) = real_main() {
        // The message already says `FAILED <run>: <why>` where a run failed.
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run(a) => one_run(a, true),
        Cmd::Rev(a) => one_run(a, false),
        Cmd::Bench {
            env,
            keys,
            secs,
            no_play,
        } => {
            let (c, k) = (env.cfg()?, keys.get("moq-subghz-key.hex"));
            let mut h = host(env.dry_run);
            let steps = matrix::bench_plan(secs, &k.lkey, &k.pkey);
            matrix::with_clock_log(&c, env.dry_run, || {
                matrix::run_matrix(&mut *h, &c, !no_play, &k, &steps, &mut |l| println!("{l}"))
            })
        }
        Cmd::Bench051 {
            env,
            keys,
            part,
            reps,
            fsk_secs,
            no_play,
        } => {
            let (c, k) = (env.cfg()?, keys.get("../moq-subghz-key.hex"));
            let mut h = host(env.dry_run);
            let steps = matrix::plan_051(part, reps, fsk_secs, &k.pkey);
            if !env.dry_run {
                std::fs::create_dir_all(&c.runs)?;
            }
            let log = c.runs.join("bench.log");
            let mut say = |l: &str| {
                let line = format!("{} {l}", matrix::stamp_now());
                println!("{line}");
                if !env.dry_run {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&log)
                    {
                        let _ = writeln!(f, "{line}");
                    }
                }
            };
            matrix::run_matrix(&mut *h, &c, !no_play, &k, &steps, &mut say)
        }
        Cmd::Lora2015Run {
            env,
            name,
            mode,
            file,
            profile,
            passes,
            args,
        } => {
            let (c, mut h) = (env.cfg()?, host(env.dry_run));
            orch::lora(
                &mut *h,
                &c,
                &orch::Lora {
                    name: &name,
                    mode: &mode,
                    file: &file,
                    profile: &profile,
                    passes,
                    args: &args,
                },
            )
        }
        Cmd::Demo051 { env, out, play } => {
            let (c, mut h) = (env.cfg()?, host(env.dry_run));
            let out = out.unwrap_or_else(|| c.runs.join("demo"));
            demo::run(&mut *h, &c, &out, play)
        }
        Cmd::Clocklog { env } => matrix::clocklog_until_file(&env.cfg()?),
        Cmd::Package051 { runs, demo, out } => package::run(&runs, &demo, &out),
        Cmd::Lora2015Table { bin, repo_root } => {
            let bin = match bin {
                Some(b) => b,
                None => std::env::current_exe()?
                    .parent()
                    .context("no directory")?
                    .to_path_buf(),
            };
            let t = lora2015::table(
                &mut exec::Real::default(),
                &bin.join("lora2015-send").display().to_string(),
                &repo_root.join("results/lora-2015/frames"),
            )?;
            println!("{t}");
            Ok(())
        }
        Cmd::Lora2015Frames { repo_root } => {
            let have = std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("ssdv").is_file()));
            lora2015::make_frames(&mut exec::Real::default(), &repo_root, have)
        }
        Cmd::Analyze {
            runs,
            out,
            repo_root,
        } => analyze::run(
            &runs,
            &out,
            &analyze::Inputs::under(&repo_root),
            &analyze::ffmpeg_psnr,
        ),
        Cmd::FactoryBench(o) => factory::run(&o),
        Cmd::StickCap(o) => stickcap::run(&o),
        Cmd::FactorySend {
            chunks,
            app_dir,
            i_have_an_antenna,
            timeout_ms,
        } => {
            anyhow::ensure!(
                i_have_an_antenna,
                "refusing to transmit without --i-have-an-antenna"
            );
            factory::send_chunks(&chunks, &app_dir, Duration::from_millis(timeout_ms))
        }
        Cmd::Elf2uf2 { elf, uf2 } => {
            let (bytes, line) = uf2::convert(
                &std::fs::read(&elf).with_context(|| format!("read {}", elf.display()))?,
            )?;
            std::fs::write(&uf2, bytes)?;
            println!("{line}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod testutil;
