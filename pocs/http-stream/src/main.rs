use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gx_contracts::{MediaType, ResolvedMediaRequest};
use gx_streaming::{HttpMediaSource, decode_http_window};

fn main() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--self-test") {
        let path = args
            .get(1)
            .map(PathBuf::from)
            .context("usage: http-stream --self-test <media-file> [seek-seconds]")?;
        let seek_seconds = args
            .get(2)
            .map(|value| value.parse::<f64>())
            .transpose()
            .context("invalid seek-seconds")?
            .unwrap_or(30.0);
        return run_self_test(path, seek_seconds);
    }

    let url = args
        .first()
        .context("usage: http-stream <url> [seek-seconds]")?;
    let seek_seconds = args
        .get(1)
        .map(|value| value.parse::<f64>())
        .transpose()
        .context("invalid seek-seconds")?
        .unwrap_or(0.0);
    let request = request_for(url)?;
    let report = decode_http_window(request, seek_seconds, 8_192)?;
    println!("{report:#?}");
    Ok(())
}

fn run_self_test(path: PathBuf, seek_seconds: f64) -> Result<()> {
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read self-test media {}", path.display()))?;
    if bytes.len() < 32 * 1024 {
        bail!("self-test media must be at least 32 KiB");
    }
    let server = SelfTestServer::start(bytes)?;
    let redirect_url = format!("http://{}/redirect", server.address());
    let media_url = format!("http://{}/media", server.address());

    let initial = decode_http_window(request_for(&redirect_url)?, 0.0, 200_000)?;
    println!("initial progressive decode: {initial:#?}");
    if initial.metrics.reconnects == 0 || initial.metrics.requests < 2 {
        bail!("forced connection drop did not exercise HTTP recovery");
    }
    if initial.final_url.ends_with("/redirect") {
        bail!("redirect was not followed to the final media URL");
    }

    let seeked = decode_http_window(request_for(&redirect_url)?, seek_seconds, 8_192)?;
    println!("range seek decode: {seeked:#?}");
    if seeked.metrics.range_requests == 0 {
        bail!("Symphonia seek did not produce an HTTP Range request");
    }

    let mut source = HttpMediaSource::new(request_for(&media_url)?)?;
    let metrics = source.metrics_handle();
    thread::sleep(Duration::from_millis(450));
    let mut byte = [0u8; 1];
    source.read_exact(&mut byte)?;
    let backpressure = metrics.snapshot();
    println!("bounded-buffer probe: {backpressure:#?}");
    if backpressure.backpressure_waits == 0 {
        bail!("bounded HTTP channel did not demonstrate backpressure");
    }

    println!(
        "self-test server requests={}, range_requests={}",
        server.requests.load(Ordering::Relaxed),
        server.range_requests.load(Ordering::Relaxed)
    );
    Ok(())
}

fn request_for(url: &str) -> Result<ResolvedMediaRequest> {
    Ok(ResolvedMediaRequest {
        url: url.parse()?,
        headers: Vec::new(),
        media_type: MediaType::Mp3,
        quality: Some("self-test".into()),
        expires_at_ms: None,
        network_route: None,
    })
}

struct SelfTestServer {
    address: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    requests: Arc<AtomicU64>,
    range_requests: Arc<AtomicU64>,
}

impl SelfTestServer {
    fn start(media: Vec<u8>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicU64::new(0));
        let range_requests = Arc::new(AtomicU64::new(0));
        let dropped_once = Arc::new(AtomicBool::new(false));
        let stop_worker = Arc::clone(&stop);
        let requests_worker = Arc::clone(&requests);
        let ranges_worker = Arc::clone(&range_requests);
        let handle = thread::spawn(move || {
            while !stop_worker.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        requests_worker.fetch_add(1, Ordering::Relaxed);
                        let _ = handle_connection(stream, &media, &dropped_once, &ranges_worker);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            address,
            stop,
            handle: Some(handle),
            requests,
            range_requests,
        })
    }

    fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for SelfTestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(&self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    media: &[u8],
    dropped_once: &AtomicBool,
    range_requests: &AtomicU64,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > 64 * 1024 {
            bail!("self-test HTTP request headers are too large");
        }
    }
    let request_text = String::from_utf8_lossy(&request);
    let path = request_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path == "/redirect" {
        stream.write_all(
            b"HTTP/1.1 302 Found\r\nLocation: /media\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    }
    if path != "/media" {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    }

    let range_start = request_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        value
            .trim()
            .strip_prefix("bytes=")?
            .strip_suffix('-')?
            .parse::<usize>()
            .ok()
    });
    let start = range_start.unwrap_or(0);
    if range_start.is_some() {
        range_requests.fetch_add(1, Ordering::Relaxed);
    }
    if start >= media.len() {
        stream.write_all(
            format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                media.len()
            )
            .as_bytes(),
        )?;
        return Ok(());
    }

    let body = &media[start..];
    if range_start.is_some() {
        stream.write_all(
            format!(
                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: audio/mpeg\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                start,
                media.len() - 1,
                media.len(),
                body.len()
            )
            .as_bytes(),
        )?;
        stream.write_all(body)?;
        return Ok(());
    }

    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            media.len()
        )
        .as_bytes(),
    )?;
    if !dropped_once.swap(true, Ordering::AcqRel) {
        let partial = body.len().min(16 * 1024);
        stream.write_all(&body[..partial])?;
        stream.flush()?;
        return Ok(());
    }
    stream.write_all(body)?;
    Ok(())
}
