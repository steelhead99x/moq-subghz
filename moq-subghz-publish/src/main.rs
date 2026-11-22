//! moq-subghz-publish: read the object records `moq-subghz-recv --publish` writes on stdin and
//! publish them as a MoQ broadcast on a moq.dev relay or the moq.pro CDN.
use anyhow::{bail, Context, Result};
use clap::Parser;
use moq_subghz_publish::{Publisher, Push};
use moq_subghz_wire::objstream::{read_record, Record};
use std::io::BufReader;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Relay URL. Local moq.dev relay: http://localhost:4443/anon.
    /// moq.pro: the URL and ?jwt= token from your project's dashboard.
    #[arg(long, env = "MOQ_SUBGHZ_RELAY")]
    url: url::Url,
    /// Broadcast name (the path viewers subscribe to)
    #[arg(long, default_value = "moq-subghz")]
    name: String,
    /// Coded picture size for the catalog, WxH (must match the sender's --size)
    #[arg(long, default_value = "320x240")]
    size: String,
    /// Pin a self-signed relay certificate by its SHA-256 (hex) instead of the system roots
    #[arg(long, env = "MOQ_SUBGHZ_TLS_FINGERPRINT")]
    tls_fingerprint: Option<String>,
    /// Read records from this file instead of stdin
    #[arg(long)]
    input: Option<String>,
    /// Keep the broadcast up this long after the input ends, so the relay gets the tail
    #[arg(long, default_value_t = 1.0)]
    linger_s: f64,
    /// Give up (exit non-zero) if no relay session is established within this many seconds
    #[arg(long, default_value_t = 10.0)]
    connect_timeout_s: f64,
    /// Wait at most this many seconds for the session to deliver the last groups and close
    #[arg(long, default_value_t = 5.0)]
    close_timeout_s: f64,
}

/// A command line duration in seconds, finite and positive.
fn secs(name: &str, v: f64) -> Result<Duration> {
    if !v.is_finite() || v <= 0.0 || v > 86_400.0 {
        bail!("{name} must be between 0 and 86400 seconds (got {v})");
    }
    Ok(Duration::from_secs_f64(v))
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').context("--size is WxH")?;
    Ok((w.parse()?, h.parse()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let size = parse_size(&a.size)?;
    let connect_timeout = secs("--connect-timeout-s", a.connect_timeout_s)?;
    let close_timeout = secs("--close-timeout-s", a.close_timeout_s)?;
    if !matches!(a.url.scheme(), "http" | "https" | "moqt" | "moql") {
        bail!(
            "--url must be http(s):// or moqt:// (got {})",
            a.url.scheme()
        );
    }

    // Records arrive on a blocking reader thread; the radio side never waits on the network.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Record>(256);
    let input = a.input.clone();
    let reader = std::thread::spawn(move || -> Result<()> {
        let src: Box<dyn std::io::Read + Send> = match input {
            Some(p) => Box::new(std::fs::File::open(&p).with_context(|| format!("open {p}"))?),
            None => Box::new(std::io::stdin()),
        };
        let mut r = BufReader::new(src);
        while let Some(rec) = read_record(&mut r)? {
            if tx.blocking_send(rec).is_err() {
                break;
            }
        }
        Ok(())
    });

    let _ = moq_tokio::crypto::install_default();
    let origin = moq_tokio::origin::spawn();
    let mut publisher = Publisher::new(&origin, &a.name, Some(size))?;

    let mut connect = moq_tokio::connect::Config::default();
    connect.url = Some(a.url.clone());
    if let Some(fp) = &a.tls_fingerprint {
        connect.tls.fingerprint = vec![fp.clone()];
    }
    let client = connect
        .init(moq_tokio::quic::Config::default())
        .context("moq client")?
        .with_publisher(&origin);
    let shown = moq_tokio::RedactedUrl::new(&a.url).to_string();
    eprintln!("moq-subghz-publish: {shown} broadcast={}", a.name);
    // Reconnects on its own; groups written while it is down are served once it is back.
    let connection = client.connect(a.url.clone());
    let connect_deadline = tokio::time::Instant::now() + connect_timeout;
    let mut established = Box::pin(connection.clone().established());
    let mut is_up = false;
    let mut announced_live = false;
    let not_up = |shown: &str, t: Duration| {
        anyhow::anyhow!(
            "no relay session with {shown} within {:.1} s: nothing was published",
            t.as_secs_f64()
        )
    };

    let mut input_open = true;
    while input_open {
        tokio::select! {
            rec = rx.recv() => {
                let Some(rec) = rec else { input_open = false; continue };
                if let Push::Started { sequence } = publisher.push(&rec)? {
                    if publisher.stats.groups == 1 && is_up {
                        eprintln!("moq-subghz-publish: first group {sequence} live");
                        announced_live = true;
                    }
                }
            }
            res = &mut established, if !is_up => {
                let _clone = res.map_err(|e| anyhow::anyhow!("relay session failed: {e}"))?;
                is_up = true;
                if publisher.stats.groups >= 1 {
                    eprintln!("moq-subghz-publish: first group live (relay session established)");
                    announced_live = true;
                }
            }
            _ = tokio::time::sleep_until(connect_deadline), if !is_up => {
                return Err(not_up(&shown, connect_timeout));
            }
            res = connection.closed() => {
                res.context("relay connection closed")?;
                bail!("relay connection closed");
            }
        }
    }
    // Input ended. Never report success without a relay session: wait out what is left of the
    // connect deadline for it.
    if !is_up {
        match tokio::time::timeout_at(connect_deadline, &mut established).await {
            Ok(Ok(_)) => {
                is_up = true;
                if publisher.stats.groups >= 1 && !announced_live {
                    eprintln!("moq-subghz-publish: first group live (relay session established)");
                }
            }
            Ok(Err(e)) => bail!("relay session failed: {e}"),
            Err(_) => return Err(not_up(&shown, connect_timeout)),
        }
    }
    debug_assert!(is_up);
    // Keep the broadcast up for a moment so viewers and the relay get the last groups, then end
    // it cleanly and close the session, which delivers what is queued, within a bound.
    tokio::time::sleep(Duration::from_secs_f64(a.linger_s.clamp(0.0, 3600.0))).await;
    publisher.finish();
    let closed = tokio::time::timeout(close_timeout, connection.close()).await;
    let close_err = match closed {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("closing the relay session failed: {e}")),
        Err(_) => Some(format!(
            "the relay session did not close within {:.1} s: the tail may be lost",
            close_timeout.as_secs_f64()
        )),
    };
    let _ = tokio::time::timeout(close_timeout, client.close()).await;
    let s = publisher.stats;
    eprintln!(
        "moq-subghz-publish: groups={} frames={} groups_cut={} skipped={}",
        s.groups, s.frames, s.groups_cut, s.skipped
    );
    match reader.join() {
        Ok(r) => r.context("reading records")?,
        Err(_) => bail!("record reader panicked"),
    }
    if let Some(e) = close_err {
        bail!("{e}");
    }
    Ok(())
}
