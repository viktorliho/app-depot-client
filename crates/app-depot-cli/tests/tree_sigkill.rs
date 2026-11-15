#![cfg(target_os = "linux")]
//! Process-level interruption against a disposable loopback Dev/Test Server fixture.
use serde_json::Value;
use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::ExitStatusExt,
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn command(bin: &str, library: &Path, args: &[&str]) -> Output {
    let output = Command::new(bin)
        .arg("--library-data")
        .arg(library)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn json(bin: &str, library: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&command(bin, library, args).stdout).unwrap()
}
fn serve_one(mut client: TcpStream, upstream: &str) {
    let mut headers = Vec::new();
    let mut buffer = [0u8; 4096];
    while !headers.windows(4).any(|window| window == b"\r\n\r\n") {
        let Ok(n) = client.read(&mut buffer) else {
            return;
        };
        if n == 0 || headers.len() + n > 65536 {
            return;
        }
        headers.extend_from_slice(&buffer[..n]);
    }
    let Ok(request) = std::str::from_utf8(&headers) else {
        return;
    };
    let Some(path) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        return;
    };
    if !path.starts_with("/api/v1/") {
        return;
    }
    let mut call = ureq::get(&format!("{}{path}", upstream.trim_end_matches('/')));
    for line in request.lines() {
        if let Some((key, value)) = line.split_once(':')
            && matches!(
                key.to_ascii_lowercase().as_str(),
                "range" | "if-range" | "authorization"
            )
        {
            call = call.set(key, value.trim());
        }
    }
    let response = match call.call() {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(_) => return,
    };
    let _ = write!(
        client,
        "HTTP/1.1 {} Test\r\nConnection: close\r\n",
        response.status()
    );
    for key in [
        "Content-Type",
        "Content-Length",
        "Content-Range",
        "ETag",
        "Accept-Ranges",
    ] {
        if let Some(value) = response.header(key) {
            let _ = write!(client, "{key}: {value}\r\n");
        }
    }
    let _ = client.write_all(b"\r\n");
    let mut reader = response.into_reader();
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        if client.write_all(&buffer[..n]).is_err() {
            break;
        }
        let _ = client.flush();
        if path.contains("/files/") {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
#[ignore = "requires isolated loopback Dev/Test Server with Published multi-file Tree fixture"]
fn sigkill_during_tree_member_then_recover_and_browse_offline() {
    let upstream =
        env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated loopback Server URL required");
    assert!(
        upstream.starts_with("http://127.0.0.1:")
            && upstream
                .trim_end_matches('/')
                .split(':')
                .next_back()
                .is_some_and(|port| port.parse::<u16>().is_ok()),
        "test requires loopback HTTP Dev Server"
    );
    let release = env::var("APP_DEPOT_TEST_RELEASE_ID").expect("fixture Release ID required");
    let association =
        env::var("APP_DEPOT_TEST_TREE_ASSOCIATION_ID").expect("Tree Association ID required");
    let bin = env!("CARGO_BIN_EXE_app-depot");
    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("library");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}/", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let upstream_thread = upstream.clone();
    let server = thread::spawn(move || {
        while !stop_thread.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let url = upstream_thread.clone();
                    thread::spawn(move || serve_one(stream, &url));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("proxy accept: {error}"),
            }
        }
    });
    let profile = json(
        bin,
        &library,
        &["profile", "add", "SIGKILL test", &proxy, "--json"],
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let initial = json(
        bin,
        &library,
        &[
            "--profile",
            &profile,
            "download",
            "start",
            &release,
            &association,
            "--pause-after-bytes",
            "1",
            "--json",
        ],
    );
    assert_eq!(initial["state"], "Paused");
    let job = initial["id"].as_str().unwrap().to_owned();
    let files = json(
        bin,
        &library,
        &[
            "--profile",
            &profile,
            "download",
            "tree-files",
            &job,
            "--json",
        ],
    );
    let large = files
        .as_array()
        .unwrap()
        .iter()
        .max_by_key(|file| file["size"].as_u64().unwrap())
        .unwrap();
    let size = large["size"].as_u64().unwrap();
    assert!(size > 262144, "fixture needs a sufficiently large file");
    let stage = library
        .join("staging")
        .join(&job)
        .join("files")
        .join(format!("{}.part", large["fileId"].as_str().unwrap()));
    let mut child = Command::new(bin)
        .arg("--library-data")
        .arg(&library)
        .args(["--profile", &profile, "download", "resume", &job, "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let killed_at = loop {
        if let Ok(meta) = fs::metadata(&stage)
            && (131072..size).contains(&meta.len())
        {
            child.kill().unwrap(); // SIGKILL on Unix
            break meta.len();
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "client exited before SIGKILL"
        );
        assert!(
            Instant::now() < deadline,
            "member did not reach a partial stage"
        );
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(child.wait().unwrap().signal(), Some(9));
    let jobs = json(
        bin,
        &library,
        &["--profile", &profile, "download", "jobs", "--json"],
    );
    let recovered = jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == job)
        .unwrap();
    assert_eq!(recovered["state"], "Paused");
    let files = json(
        bin,
        &library,
        &[
            "--profile",
            &profile,
            "download",
            "tree-files",
            &job,
            "--json",
        ],
    );
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["verified"] == true)
    );
    let final_job = json(
        bin,
        &library,
        &["--profile", &profile, "download", "resume", &job, "--json"],
    );
    assert_eq!(final_job["state"], "Completed");
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    let trees = json(
        bin,
        &library,
        &["--profile", &profile, "download", "trees", "--json"],
    );
    assert_eq!(trees.as_array().unwrap().len(), 1);
    assert!(
        Path::new(trees[0]["path"].as_str().unwrap())
            .join("empty/nested")
            .is_dir()
    );
    eprintln!("SIGKILL at {killed_at} bytes; recovered and verified Tree offline");
}
