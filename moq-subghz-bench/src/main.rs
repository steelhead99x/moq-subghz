//! Bench and analysis tools for moq-subghz: the bench harness and its analysis.

mod analyze;
mod cell;
mod css2015;
mod demo;
mod exec;
mod factory;
mod licence;
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
    /// Build the CSVs and charts of results/stick-0.5.1 from the raw runs
    Analyze {
        /// Directory of the raw runs
        runs: PathBuf,
        /// Where the CSVs and SVGs go
        out: PathBuf,
        /// Root of this repository, for the reference pictures and the factory results
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// Factory-vs-factory picture test: the HAT maker's stock app on the board, the stock receiver on the stick
    FactoryBench(factory::Opts),
    /// Raw logger for the stock receiver sketch on the stick (opens only --dev)
    StickCap(stickcap::Opts),
    /// The board side of factory-bench: send each chunk with the HAT maker's stock app
    FactorySend {
        /// Text file with one chunk per line
        chunks: PathBuf,
        /// Directory of the stock `hat_app`
        #[arg(long, default_value = ".")]
        app_dir: PathBuf,
        /// Required: the antenna is on the HAT
        #[arg(long)]
        i_have_an_antenna: bool,
        /// The carrier the stock app is built for, in MHz. No default: the app cannot be queried,
        /// so you declare it and the licence gate checks it (the stock build's 868.1 is refused)
        #[arg(long)]
        freq_mhz: f64,
        /// The power the stock app is built for, in dBm (the project allows 2 to 5)
        #[arg(long)]
        power_dbm: i8,
        /// How long each send may run before it is cut off, in milliseconds
        #[arg(long, default_value_t = 1300)]
        timeout_ms: u64,
        #[command(flatten)]
        gate: moq_subghz_licence::GateArgs,
    },
    /// One Pi-to-laptop run: receiver here, sender on the board
    Run(RunArgs),
    /// One laptop-to-Pi run: receiver on the board, sender here
    Rev(RunArgs),
    /// The matched rind off/on matrix of the README, 8 runs
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
    /// The whole results/stick-0.5.1 matrix
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
    /// One css2015 picture run: logger here, sender on the board, exact loss from a replay
    Css2015Run {
        #[command(flatten)]
        env: orch::Env,
        name: String,
        /// chunk, ssdv or framed
        mode: String,
        /// A file name under results/css-2015/frames/
        file: String,
        profile: String,
        passes: u32,
        /// Sender arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The 6 minute guided demo of the stick
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
    /// Log the board-minus-laptop clock offset every 5 s until RUNS/clocklog.stop exists
    Clocklog {
        #[command(flatten)]
        env: orch::Env,
    },
    /// Copy the data worth keeping from a bench-051 runs directory
    #[command(name = "package-051")]
    Package051 {
        runs: PathBuf,
        demo: PathBuf,
        out: PathBuf,
        /// Licence file whose ID and name must not appear in what is copied (default: the
        /// MOQ_SUBGHZ_LICENCE or XDG location)
        #[arg(long, value_name = "PATH", conflicts_with = "no_licence")]
        licence_file: Option<PathBuf>,
        /// The runs were made unlicensed: nothing configured to scan for (unredacted
        /// licence lines in the summaries are still refused)
        #[arg(long)]
        no_licence: bool,
    },
    /// Print the calculated airtime table of results/css-2015/README.md
    Css2015Table {
        /// Directory of the built binaries (default: the directory of this one)
        #[arg(long, env = "BIN")]
        bin: Option<PathBuf>,
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// Rebuild the test pictures of results/css-2015/frames
    Css2015Frames {
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/.."))]
        repo_root: PathBuf,
    },
    /// ELF to UF2 for the RP2040
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
    /// Path of the same key on the board
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
    c.announce();
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
        // 130: stopped by a signal, as a shell reports Ctrl-C
        std::process::exit(if e.downcast_ref::<factory::Cancelled>().is_some() {
            130
        } else {
            1
        });
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
            c.announce();
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
            c.announce();
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
        Cmd::Css2015Run {
            env,
            name,
            mode,
            file,
            profile,
            passes,
            args,
        } => {
            let (c, mut h) = (env.cfg()?, host(env.dry_run));
            c.announce();
            orch::css(
                &mut *h,
                &c,
                &orch::Css {
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
            c.announce();
            let out = out.unwrap_or_else(|| c.runs.join("demo"));
            demo::run(&mut *h, &c, &out, play)
        }
        Cmd::Clocklog { env } => {
            let c = env.cfg()?;
            if env.dry_run {
                // One plan, nothing written: no stop file removed, no directory made, no loop.
                return matrix::clocklog_dry(&mut exec::Dry::new(), &c);
            }
            matrix::clocklog_until_file(&c)
        }
        Cmd::Package051 {
            runs,
            demo,
            out,
            licence_file,
            no_licence,
        } => {
            let g = package::Guard::load(licence_file, no_licence)?;
            package::run(&g, &runs, &demo, &out)
        }
        Cmd::Css2015Table { bin, repo_root } => {
            let bin = match bin {
                Some(b) => b,
                None => std::env::current_exe()?
                    .parent()
                    .context("no directory")?
                    .to_path_buf(),
            };
            let t = css2015::table(
                &mut exec::Real::default(),
                &bin.join("css2015-send").display().to_string(),
                &repo_root.join("results/css-2015/frames"),
            )?;
            println!("{t}");
            Ok(())
        }
        Cmd::Css2015Frames { repo_root } => {
            let have = std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("ssdv").is_file()));
            css2015::make_frames(&mut exec::Real::default(), &repo_root, have)
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
            freq_mhz,
            power_dbm,
            timeout_ms,
            gate,
        } => {
            // The gate first: before the chunk file is read or the stock app started.
            let station = gate.station().context("licence")?;
            let permit = factory::gate(
                &station,
                factory::Declared {
                    freq_mhz,
                    power_dbm,
                },
            )?;
            anyhow::ensure!(
                i_have_an_antenna,
                "refusing to transmit without --i-have-an-antenna"
            );
            factory::send_chunks(
                &permit,
                &chunks,
                &app_dir,
                Duration::from_millis(timeout_ms),
                factory::cancel_flag(),
                &mut std::io::stdout().lock(),
            )
        }
        Cmd::Elf2uf2 { elf, uf2 } => {
            let (bytes, line) = uf2::convert(&cell::read_capped(&elf, cell::MAX_ELF)?)?;
            std::fs::write(&uf2, bytes)?;
            println!("{line}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod testutil;
