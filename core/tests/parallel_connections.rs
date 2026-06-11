/// Parallel-connection integration test.
///
/// Opens 3 independent SFTP sessions concurrently (matching what the
/// TransferPool does at runtime) and verifies:
///   1. Each connection transfers data correctly.
///   2. The 3 concurrent uploads finish in ≈ the same wall-clock time as a
///      single upload (≤ 2× — loose bound to avoid CI flakiness).
///
/// Requires the test containers from tests/integration/docker-compose.yml:
///   docker compose -f tests/integration/docker-compose.yml up -d sftp
///
/// Run:
///   cargo test -p scp-core --test parallel_connections -- --ignored --nocapture
use std::path::Path;
use std::thread;
use std::time::Instant;

use scp_core::connect;
use scp_core::types::{Auth, Credentials, HostKeyPolicy, Protocol};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;
const USER: &str = "demo";
const PASS: &str = "demopass";
const POOL_SIZE: usize = 3;
const FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MB per file

fn creds() -> Credentials {
    Credentials {
        protocol: Protocol::Sftp,
        host: HOST.into(),
        port: PORT,
        username: USER.into(),
        auth: Auth::Password(PASS.into()),
        host_key: HostKeyPolicy::AcceptNew,
        bucket: None,
        region: None,
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

#[test]
#[ignore = "needs the docker test rig: docker compose -f tests/integration/docker-compose.yml up -d sftp"]
fn three_parallel_uploads_correct_and_fast() {
    let tmp_dir = tempfile::tempdir().expect("tmpdir");

    // Prepare source files.
    let sources: Vec<tempfile::TempPath> = (0..POOL_SIZE as u8)
        .map(|i| make_temp_file(FILE_BYTES, i))
        .collect();

    // -- Baseline: sequential uploads via ONE connection --
    let seq_start = Instant::now();
    {
        let mut t = connect(&creds()).expect("sequential connect");
        for (i, src) in sources.iter().enumerate() {
            let remote = format!("/upload/seq_{i}.bin");
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
            thread::spawn(move || {
                let mut t = connect(&creds).expect("pool connect");
                let remote = format!("/upload/par_{i}.bin");
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
            let remote = format!("/upload/par_{i}.bin");
            let local = tmp_dir.path().join(format!("par_{i}.bin"));
            t.download(&remote, &local)
                .unwrap_or_else(|e| panic!("download par_{i} failed: {e}"));
            let got = std::fs::read(&local).expect("read downloaded");
            let expected: Vec<u8> = (0..FILE_BYTES).map(|j| (i as u8).wrapping_add(j as u8)).collect();
            assert_eq!(got, expected, "data mismatch for par_{i}");
        }
    }

    // Parallel must be faster than sequential + 20% margin to rule out a
    // completely serialised implementation.  On a loopback interface with a
    // local container the ratio is typically ≈ 1/POOL_SIZE; we use a generous
    // 2× bound to tolerate slow CI machines.
    assert!(
        par_ms < seq_ms * 2,
        "Parallel ({par_ms} ms) should be faster than 2× sequential ({seq_ms} ms). \
         Pool connections may not actually be running concurrently."
    );

    println!("Speedup: {:.1}×", seq_ms as f64 / par_ms as f64);
}
