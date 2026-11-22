//! End to end over a real MoQ session: the `moq-subghz-publish` binary dials a local
//! moq-tokio server that relays between sessions (a stand-in for moq-relay), and a second
//! session subscribes and checks every group and frame.
//!
//! `replay_through_recv` (ignored by default) runs the whole path from radio frames:
//! `moq-subghz-recv --replay --publish`. See docs/MOQ.md.

use hang::catalog::Catalog;
use hang::container::Frame;
use moq_subghz_publish::{test_au, VIDEO_TRACK};
use moq_subghz_wire::objstream::Record;
use std::process::{Command, Stdio};
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

struct Relay {
    url: url::Url,
    /// Empty for an http:// relay, which serves its own fingerprint.
    fingerprint: String,
    _task: Option<tokio::task::JoinHandle<()>>,
}

/// Every session publishes into and subscribes from one shared origin.
/// MOQ_SUBGHZ_TEST_RELAY=http://localhost:4443/anon uses a running moq-relay instead.
async fn relay() -> Relay {
    let _ = moq_tokio::crypto::install_default();
    if let Ok(url) = std::env::var("MOQ_SUBGHZ_TEST_RELAY") {
        return Relay {
            url: url.parse().unwrap(),
            fingerprint: String::new(),
            _task: None,
        };
    }
    let origin = moq_tokio::origin::spawn();
    let mut cfg = moq_tokio::listen::Config::default();
    cfg.bind = Some("127.0.0.1:0".parse().unwrap());
    cfg.tls.generate = vec!["localhost".into()];
    let server = cfg.init(Default::default()).unwrap();
    let mut listener = server.listen().await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let fingerprint = listener.certificates().fingerprints()[0].clone();
    let task = tokio::spawn(async move {
        while let Some(req) = listener.accept().await {
            let origin = origin.clone();
            tokio::spawn(async move {
                if let Ok(session) = req
                    .with_publisher(origin.consume())
                    .with_subscriber(origin)
                    .ok()
                    .await
                {
                    let _ = session.closed().await;
                }
            });
        }
    });
    Relay {
        url: format!("moqt://localhost:{port}").parse().unwrap(),
        fingerprint,
        _task: Some(task),
    }
}

/// Subscribe to `name` on the relay; return the catalog and `groups` video groups, each
/// as (sequence, frames), sorted by sequence.
async fn watch(relay: &Relay, name: &str, groups: usize) -> (Catalog, Vec<(u64, Vec<Frame>)>) {
    let origin = moq_tokio::origin::spawn();
    let mut connect = moq_tokio::connect::Config::default();
    if !relay.fingerprint.is_empty() {
        connect.tls.fingerprint = vec![relay.fingerprint.clone()];
    }
    let client = connect
        .init(Default::default())
        .unwrap()
        .with_subscriber(origin.clone());
    let conn = client.connect(relay.url.clone());
    let consumer = origin.consume();
    let mut announced = consumer.announced();
    loop {
        let u = tokio::time::timeout(T, announced.next())
            .await
            .expect("broadcast announced")
            .expect("origin open");
        if u.prefix.as_str() == name && u.kind.is_active() {
            break;
        }
    }
    let bc = tokio::time::timeout(T, consumer.request_broadcast(name))
        .await
        .expect("broadcast announced")
        .unwrap();
    let mut cat = bc
        .track(Catalog::DEFAULT_NAME)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut g = tokio::time::timeout(T, cat.recv_group())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let f = g.read_frame().await.unwrap().unwrap();
    let catalog: Catalog = Catalog::from_slice(&f.payload).unwrap();

    let mut video = bc
        .track(VIDEO_TRACK)
        .unwrap()
        .subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(30)))
        .await
        .unwrap();
    let mut out = Vec::new();
    while out.len() < groups {
        let mut g = tokio::time::timeout(T, video.recv_group())
            .await
            .expect("video group")
            .unwrap()
            .expect("track open");
        let mut frames = Vec::new();
        while let Some(f) = tokio::time::timeout(T, g.read_frame())
            .await
            .unwrap()
            .unwrap()
        {
            frames.push(Frame::decode(f.payload).unwrap());
        }
        out.push((g.sequence, frames));
    }
    drop(conn);
    out.sort_by_key(|g| g.0);
    (catalog, out)
}

fn records() -> Vec<Record> {
    let mut v = Vec::new();
    for g in 0..4u16 {
        for o in 0..5u8 {
            // Group 2 loses object 3: the publisher sends 0..=2 and closes the group.
            if g == 2 && o == 3 {
                continue;
            }
            v.push(Record {
                group: g,
                obj: o,
                key: o == 0,
                frame_no: g as u32 * 5 + o as u32,
                cap_us: 1_000_000 + (g as u64 * 5 + o as u64) * 66_666,
                payload: test_au(o == 0, (g * 16) as u8 + o),
            });
        }
    }
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_binary_through_a_relay() {
    let relay = relay().await;
    let recs = records();
    let dir = std::env::temp_dir().join(format!("moq-subghz-publish-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("objects.bin");
    std::fs::write(
        &input,
        recs.iter().flat_map(|r| r.encode()).collect::<Vec<u8>>(),
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_moq-subghz-publish"));
    cmd.args([
        "--url",
        relay.url.as_str(),
        "--name",
        "css/test",
        "--linger-s",
        "3",
    ]);
    if !relay.fingerprint.is_empty() {
        cmd.args(["--tls-fingerprint", &relay.fingerprint]);
    }
    let child = cmd
        .arg("--input")
        .arg(&input)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (catalog, groups) = watch(&relay, "css/test", 4).await;
    let out = child.wait_with_output().unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("live"),
        "live is reported once the session is up: {err}"
    );

    let video = &catalog.video.renditions[VIDEO_TRACK];
    assert_eq!(video.codec.to_string(), "avc3.42c01e");
    assert_eq!(video.coded_width, Some(320));
    assert_eq!(
        groups.iter().map(|g| g.0).collect::<Vec<_>>(),
        [0, 1, 2, 3],
        "moq-subghz group g is MoQ group g"
    );
    for (seq, frames) in &groups {
        let want: Vec<&Record> = recs
            .iter()
            .filter(|r| r.group as u64 == *seq && !(r.group == 2 && r.obj > 3))
            .collect();
        assert_eq!(frames.len(), want.len(), "group {seq}");
        for (f, r) in frames.iter().zip(want) {
            assert_eq!(f.payload.as_ref(), &r.payload[..]);
            assert_eq!(f.timestamp.as_micros(), (r.cap_us - 1_000_000) as u128);
        }
    }
}

/// The full radio-side path: `R` lines from `moq-subghz-send --loopback` replayed by
/// `moq-subghz-recv --publish`, which spawns this crate's binary. Checks that the frames a
/// viewer gets are byte for byte the access units recv recorded in rec.h264.
///
/// MOQ_SUBGHZ_RECV=path/to/moq-subghz-recv MOQ_SUBGHZ_REPLAY=loop.txt \
///   cargo test --test relay -- --ignored
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a moq-subghz-recv binary and a loopback capture"]
async fn replay_through_recv() {
    let recv = std::env::var("MOQ_SUBGHZ_RECV").expect("MOQ_SUBGHZ_RECV");
    let replay = std::env::var("MOQ_SUBGHZ_REPLAY").expect("MOQ_SUBGHZ_REPLAY");
    let groups: usize = std::env::var("MOQ_SUBGHZ_GROUPS")
        .ok()
        .and_then(|g| g.parse().ok())
        .unwrap_or(3);
    let relay = relay().await;
    let out = std::env::temp_dir().join(format!("moq-subghz-recv-{}", std::process::id()));
    let child = Command::new(recv)
        .args(["--replay", &replay, "--paced", "--secs", "30", "--publish"])
        .arg(relay.url.as_str())
        .args(["--publish-name", "css/replay", "--publish-bin"])
        .arg(env!("CARGO_BIN_EXE_moq-subghz-publish"))
        .arg("--out")
        .arg(&out)
        .envs(
            Some(("MOQ_SUBGHZ_TLS_FINGERPRINT", &relay.fingerprint)).filter(|(_, f)| !f.is_empty()),
        )
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let (catalog, got) = watch(&relay, "css/replay", groups).await;
    let status = child.wait_with_output().unwrap().status;
    assert!(status.success());
    assert!(catalog.video.renditions.contains_key(VIDEO_TRACK));
    let rec = std::fs::read(out.join("rec.h264")).unwrap();
    let mut pos = 0;
    let mut frames = 0;
    for (seq, fr) in &got {
        assert!(!fr.is_empty(), "group {seq} empty");
        for f in fr {
            let at = rec[pos..]
                .windows(f.payload.len())
                .position(|w| w == f.payload.as_ref())
                .expect("frame is in rec.h264, in order");
            pos += at + f.payload.len();
            frames += 1;
        }
    }
    eprintln!(
        "replay: {} groups ({:?}), {frames} frames matched rec.h264",
        got.len(),
        got.iter().map(|g| g.0).collect::<Vec<_>>()
    );
    std::fs::remove_dir_all(&out).ok();
}

/// A localhost UDP port nothing listens on.
fn closed_port() -> u16 {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    s.local_addr().unwrap().port()
}

fn write_records(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("moq-subghz-publish-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("objects.bin");
    std::fs::write(
        &input,
        records()
            .iter()
            .flat_map(|r| r.encode())
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    (dir, input)
}

/// With a valid H.264 record and no relay behind the port, the publisher must not say "live"
/// and must exit non-zero within the connect deadline (it used to log "first group 0 live" and
/// exit 0 while the client was still retrying).
#[test]
fn an_unserved_port_fails_instead_of_reporting_success() {
    let (dir, input) = write_records("closed");
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_moq-subghz-publish"))
        .args(["--url", &format!("moqt://localhost:{}", closed_port())])
        .args(["--connect-timeout-s", "2", "--linger-s", "0.1", "--input"])
        .arg(&input)
        .stdout(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    std::fs::remove_dir_all(&dir).ok();
    assert!(!out.status.success(), "must fail: {err}");
    assert!(
        !err.contains("live"),
        "no success claim without a session: {err}"
    );
    assert!(
        err.contains("relay session") || err.contains("relay connection"),
        "clear message: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "bounded by the deadline"
    );
}

/// A relay that accepts QUIC but never completes the MoQ session also fails within the bound.
#[test]
fn a_relay_that_never_accepts_fails_within_the_deadline() {
    let (dir, input) = write_records("mute");
    // a UDP socket that is bound but never answers: the QUIC handshake gets no reply
    let mute = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = mute.local_addr().unwrap().port();
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_moq-subghz-publish"))
        .args(["--url", &format!("moqt://localhost:{port}")])
        .args(["--connect-timeout-s", "2", "--linger-s", "0.1", "--input"])
        .arg(&input)
        .stdout(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    std::fs::remove_dir_all(&dir).ok();
    drop(mute);
    assert!(!out.status.success(), "must fail: {err}");
    assert!(!err.contains("live"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn bad_deadlines_are_refused() {
    let (dir, input) = write_records("badarg");
    let out = Command::new(env!("CARGO_BIN_EXE_moq-subghz-publish"))
        .args([
            "--url",
            "moqt://localhost:1",
            "--connect-timeout-s",
            "0",
            "--input",
        ])
        .arg(&input)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--connect-timeout-s"));
}
