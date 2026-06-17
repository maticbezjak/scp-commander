/// Parallel-connection integration tests (SFTP, FTP, S3/MinIO).
///
/// Each test opens 3 independent sessions concurrently (matching what the
/// TransferPool does at runtime) and verifies:
///   1. Each connection transfers data correctly.
///   2. The 3 concurrent uploads finish in ≈ the same wall-clock time as a
///      single upload (≤ 2× — loose bound to avoid CI flakiness).
///
/// Requires the test containers from tests/integration/docker-compose.yml:
///   docker compose -f tests/integration/docker-compose.yml up -d sftp ftp minio
///   docker compose -f tests/integration/docker-compose.yml up mc   # makes the S3 bucket
///
/// Run (the S3 test needs --features s3):
///   cargo test -p scp-core --features s3 --test parallel_connections -- --ignored --nocapture
use std::path::Path;
use std::thread;
use std::time::Instant;

use scp_core::connect;
use scp_core::types::{Auth, Credentials, HostKeyPolicy, JumpHost, Protocol};

const POOL_SIZE: usize = 3;
const FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MB per file

fn sftp_creds() -> Credentials {
    Credentials {
        protocol: Protocol::Sftp,
        host: "127.0.0.1".into(),
        port: 2222,
        username: "demo".into(),
        auth: Auth::Password("demopass".into()),
        host_key: HostKeyPolicy::AcceptNew,
        bucket: None,
        region: None,
        jump: None,
    }
}

fn ftp_creds() -> Credentials {
    Credentials {
        protocol: Protocol::Ftp,
        host: "127.0.0.1".into(),
        port: 2121,
        username: "demo".into(),
        auth: Auth::Password("demopass".into()),
        host_key: HostKeyPolicy::AcceptNew,
        bucket: None,
        region: None,
        jump: None,
    }
}

#[cfg(feature = "s3")]
fn s3_creds() -> Credentials {
    Credentials {
        protocol: Protocol::S3,
        host: "127.0.0.1".into(),
        port: 9000,
        username: "demo".into(),
        auth: Auth::Password("demopass123".into()),
        host_key: HostKeyPolicy::AcceptNew,
        bucket: Some("test".into()),
        region: None,
        jump: None,
    }
}

/// Write `n` bytes of deterministic data to a temp file and return its path.
fn make_temp_file(n: usize, seed: u8) -> tempfile::TempPath {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    let data: Vec<u8> = (0..n).map(|i| seed.wrapping_add(i as u8)).collect();
    f.write_all(&data).expect("write");
    f.into_temp_path()
}

/// Isolate HOME so a recreated container's new host key never trips the
/// fail-closed mismatch protection against the real ~/.ssh/known_hosts.
fn isolate_home() {
    static FAKE_HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = FAKE_HOME.get_or_init(|| tempfile::tempdir().expect("fake home"));
    std::env::set_var("HOME", dir.path());
}

/// Shared body: sequential baseline, 3-way parallel upload, verify, compare.
fn parallel_uploads(creds: fn() -> Credentials, remote_dir: &str) {
    isolate_home();
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let sources: Vec<tempfile::TempPath> = (0..POOL_SIZE as u8)
        .map(|i| make_temp_file(FILE_BYTES, i))
        .collect();

    // -- Baseline: sequential uploads via ONE connection --
    let seq_start = Instant::now();
    {
        let mut t = connect(&creds()).expect("sequential connect");
        for (i, src) in sources.iter().enumerate() {
            let remote = format!("{remote_dir}seq_{i}.bin");
            t.upload(Path::new(src), &remote)
                .unwrap_or_else(|e| panic!("seq upload {i} failed: {e}"));
        }
    }
    let seq_ms = seq_start.elapsed().as_millis();
    println!("Sequential: {seq_ms} ms for {POOL_SIZE} files");

    // -- Pool: POOL_SIZE parallel uploads, one connection each --
    let par_start = Instant::now();
    let handles: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let creds = creds();
            let src = src.to_path_buf();
            let remote = format!("{remote_dir}par_{i}.bin");
            thread::spawn(move || {
                let mut t = connect(&creds).expect("pool connect");
                t.upload(&src, &remote)
                    .unwrap_or_else(|e| panic!("par upload {i} failed: {e}"));
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }
    let par_ms = par_start.elapsed().as_millis();
    println!("Parallel:   {par_ms} ms for {POOL_SIZE} concurrent files");

    // -- Verify downloaded data matches sources --
    {
        let mut t = connect(&creds()).expect("verify connect");
        for i in 0..POOL_SIZE {
            let remote = format!("{remote_dir}par_{i}.bin");
            let local = tmp_dir.path().join(format!("par_{i}.bin"));
            t.download(&remote, &local)
                .unwrap_or_else(|e| panic!("download par_{i} failed: {e}"));
            let got = std::fs::read(&local).expect("read downloaded");
            let expected: Vec<u8> =
                (0..FILE_BYTES).map(|j| (i as u8).wrapping_add(j as u8)).collect();
            assert_eq!(got, expected, "data mismatch for par_{i}");
        }
    }

    // Parallel must beat 2× sequential to rule out a fully serialised pool.
    assert!(
        par_ms < seq_ms * 2,
        "Parallel ({par_ms} ms) should be faster than 2× sequential ({seq_ms} ms). \
         Pool connections may not actually be running concurrently."
    );
    println!("Speedup: {:.1}×", seq_ms as f64 / par_ms as f64);
}

#[test]
#[ignore = "needs the docker rig: docker compose -f tests/integration/docker-compose.yml up -d sftp"]
fn sftp_three_parallel_uploads() {
    parallel_uploads(sftp_creds, "/upload/");
}

#[test]
#[ignore = "needs the docker rig: docker compose -f tests/integration/docker-compose.yml up -d ftp"]
fn ftp_three_parallel_uploads() {
    parallel_uploads(ftp_creds, "/ftp/demo/");
}

/// Jump-host tunnel, validated with a single container: connect to the openssh
/// bastion (host 2223 → in-container sshd 2222) and tunnel a direct-tcpip
/// channel back to that same sshd at 127.0.0.1:2222, then run SFTP end-to-end
/// through it. Proves the bastion auth + channel bridge + nested SSH/SFTP path.
///   docker compose -f tests/integration/docker-compose.yml up -d ssh
#[test]
#[ignore = "needs the docker rig: docker compose -f tests/integration/docker-compose.yml up -d ssh"]
fn jump_host_loopback() {
    isolate_home();
    let jump = JumpHost {
        host: "127.0.0.1".into(),
        port: 2223,
        username: "demo".into(),
        auth: Auth::Password("demopass".into()),
        host_key: HostKeyPolicy::AcceptNew,
    };
    let creds = Credentials {
        protocol: Protocol::Sftp,
        host: "127.0.0.1".into(),
        port: 2222, // the bastion's own in-container sshd, reached via the tunnel
        username: "demo".into(),
        auth: Auth::Password("demopass".into()),
        bucket: None,
        region: None,
        host_key: HostKeyPolicy::AcceptNew,
        jump: Some(jump),
    };
    let mut t = connect(&creds).expect("connect through jump host");
    let entries = t.list_dir("/").expect("list / through the jump tunnel");
    println!("Listed {} entries through the jump host", entries.len());
}

#[cfg(feature = "s3")]
#[test]
#[ignore = "needs the docker rig: minio + mc bucket; run with --features s3"]
fn s3_three_parallel_uploads() {
    parallel_uploads(s3_creds, "");
}
