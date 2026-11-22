//! Bench and analysis tools for moq-subghz: the Rust replacement for the `scripts/` harness.

mod analyze;
mod cell;
mod exec;
mod factory;
mod stickcap;
mod uf2;

use anyhow::{Context, Result};
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
    /// ELF to UF2 for the RP2040 (firmware/catwan-fw/elf2uf2.py)
    Elf2uf2 { elf: PathBuf, uf2: PathBuf },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
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
