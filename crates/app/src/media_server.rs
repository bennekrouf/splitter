//! A tiny HTTP server on 127.0.0.1 that lets the web view stream the video being edited.
//!
//! The web view's media stack reads a file served through the app's own protocol in thousands
//! of tiny ranges, each answered on the UI thread, and the picture stalls. Over plain HTTP it
//! buffers normally, and serving happens on threads of its own.
//!
//! Only files registered with `publish` are served, each under a random token, and only to
//! the loopback address.
//!
//! Some web views may refuse to load media from 127.0.0.1 (Chromium, and so WebView2 on
//! Windows, is starting to restrict pages reaching local addresses). The same files are then
//! served through the app's own page protocol instead (`in_app_path`, `in_app_response`):
//! slower in WebKit, which asks for it in tiny pieces on the UI thread, but same-origin.

use dioxus::desktop::wry::http::{header, Request, Response, StatusCode};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct Server {
    port: u16,
    files: Mutex<HashMap<String, PathBuf>>,
}

static SERVER: OnceLock<Option<Server>> = OnceLock::new();

fn server() -> Option<&'static Server> {
    SERVER
        .get_or_init(|| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
            let port = listener.local_addr().ok()?.port();
            std::thread::Builder::new()
                .name("splitter-media".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        std::thread::spawn(move || {
                            let _ = handle(stream);
                        });
                    }
                })
                .ok()?;
            Some(Server { port, files: Mutex::new(HashMap::new()) })
        })
        .as_ref()
}

/// Serve `path` (replacing whatever was published before) and return its URL.
pub fn publish(path: &Path) -> Option<String> {
    let server = server()?;
    let token = random_token();
    let mut files = server.files.lock().unwrap();
    files.clear();
    files.insert(token.clone(), path.to_owned());
    let name = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    Some(format!("http://127.0.0.1:{}/{token}.{name}", server.port))
}

/// Stop serving what `publish` returned `url` for (e.g. when another file is selected).
pub fn unpublish(url: &str) {
    if let Some(server) = server() {
        server.files.lock().unwrap().remove(token_of(url));
    }
}

/// Longest byte range answered at once through the app's own protocol, which holds it in
/// memory; the web view asks again for the rest.
const IN_APP_CHUNK: u64 = 4 << 20;

/// The same-origin path, for the app's own protocol, of what `publish` returned `url` for.
pub fn in_app_path(url: &str) -> String {
    format!("/media/{}", url.rsplit('/').next().unwrap_or(""))
}

/// Answer a request for an `in_app_path` (register it for "media" with `use_asset_handler`).
/// Reads the file, so call it off the UI thread.
pub fn in_app_response(request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let empty = |status| Response::builder().status(status).body(Cow::Borrowed(&[][..])).unwrap();
    let path = server().and_then(|s| s.files.lock().unwrap().get(token_of(request.uri().path())).cloned());
    let Some(path) = path else { return empty(StatusCode::NOT_FOUND) };
    let Ok(mut file) = File::open(&path) else { return empty(StatusCode::NOT_FOUND) };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return empty(StatusCode::INTERNAL_SERVER_ERROR) };
    let range = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let Some((start, end)) = byte_range(range, len) else {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Cow::Borrowed(&[][..]))
            .unwrap();
    };
    // Always partial content (even without a Range header), so the web view knows it can seek.
    let end = end.min(start + IN_APP_CHUNK - 1);
    let mut body = vec![0; (end - start + 1) as usize];
    if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut body)).is_err() {
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, content_type(&path))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .body(Cow::Owned(body))
        .unwrap()
}

/// The token in a URL path (`/<token>.<ext>`).
fn token_of(target: &str) -> &str {
    target.rsplit('/').next().unwrap_or("").split(['.', '?']).next().unwrap_or("")
}

/// 128 bits from the OS's hash seeds: not guessable by other local programs.
fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

fn content_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // Chromium (WebView2) won't try "video/quicktime", but plays most MOVs as MP4.
    match ext.as_str() {
        "mov" if cfg!(target_os = "macos") => "video/quicktime",
        _ => "video/mp4",
    }
}

/// Answer requests on one connection until the client closes it (or goes away mid-body,
/// which the web view does whenever it has buffered enough).
fn handle(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut range = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("range") {
                    range = Some(value.trim().to_string());
                }
            }
        }
        let mut parts = request_line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let path = server().and_then(|s| s.files.lock().unwrap().get(token_of(target)).cloned());
        match (method, path) {
            ("GET" | "HEAD", Some(path)) => serve(&mut out, &path, range.as_deref(), method == "HEAD")?,
            _ => status(&mut out, "404 Not Found")?,
        }
    }
}

fn status(out: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(out, "HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n")
}

fn serve(out: &mut TcpStream, path: &Path, range: Option<&str>, head: bool) -> std::io::Result<()> {
    let Ok(mut file) = File::open(path) else { return status(out, "404 Not Found") };
    let len = file.metadata()?.len();
    let Some((start, end)) = byte_range(range, len) else {
        return write!(
            out,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\n\r\n"
        );
    };
    let size = end - start + 1;
    let first = match range {
        Some(_) => format!("206 Partial Content\r\nContent-Range: bytes {start}-{end}/{len}"),
        None => "200 OK".into(),
    };
    write!(
        out,
        "HTTP/1.1 {first}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\nContent-Length: {size}\r\n\
         Cache-Control: no-store\r\n\r\n",
        content_type(path)
    )?;
    if !head {
        file.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut file.take(size), out)?;
    }
    out.flush()
}

/// The inclusive byte range `[start, end]` for a `Range` header on a `len`-byte file. No header
/// means the whole file; `None` if it can't be satisfied.
fn byte_range(header: Option<&str>, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let (start, end) = match header {
        None => (0, len - 1),
        Some(h) => {
            // Only the first range of a multi-range request; web views never ask for more.
            let spec = h.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
            let (a, b) = spec.split_once('-')?;
            match (a.trim(), b.trim()) {
                // Suffix: the last `n` bytes.
                ("", n) => {
                    let n: u64 = n.parse().ok()?;
                    if n == 0 {
                        return None;
                    }
                    (len.saturating_sub(n), len - 1)
                }
                (a, "") => (a.parse().ok()?, len - 1),
                (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(len - 1)),
            }
        }
    };
    (start <= end && start < len).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges() {
        assert_eq!(byte_range(Some("bytes=0-1"), 100), Some((0, 1)));
        assert_eq!(byte_range(Some("bytes=10-"), 100), Some((10, 99)));
        assert_eq!(byte_range(Some("bytes=90-200"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=-10"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=5-9, 20-30"), 100), Some((5, 9)));
        assert_eq!(byte_range(None, 100), Some((0, 99)));
        assert_eq!(byte_range(Some("bytes=100-"), 100), None);
        assert_eq!(byte_range(Some("bytes=9-5"), 100), None);
        assert_eq!(byte_range(Some("items=0-1"), 100), None);
        assert_eq!(byte_range(Some("bytes=0-"), 0), None);
    }

    #[test]
    fn serves_only_published_files_with_ranges() {
        let path = std::env::temp_dir().join("splitter-media-test.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let url = publish(&path).unwrap();
        let get = |url: &str, range: Option<&str>| {
            let rest = url.strip_prefix("http://").unwrap();
            let (host, target) = rest.split_once('/').unwrap();
            let mut s = TcpStream::connect(host).unwrap();
            let range = range.map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
            write!(s, "GET /{target} HTTP/1.1\r\nHost: {host}\r\n{range}Connection: close\r\n\r\n").unwrap();
            s.shutdown(std::net::Shutdown::Write).unwrap();
            let mut resp = String::new();
            s.read_to_string(&mut resp).unwrap();
            resp
        };
        let r = get(&url, Some("bytes=2-4"));
        assert!(r.starts_with("HTTP/1.1 206"), "{r}");
        assert!(r.contains("Content-Range: bytes 2-4/10"), "{r}");
        assert!(r.ends_with("\r\n\r\n234"), "{r}");
        assert!(get(&url, None).ends_with("0123456789"));

        let (base, _) = url.rsplit_once('/').unwrap();
        assert!(get(&format!("{base}/guess.mp4"), None).starts_with("HTTP/1.1 404"));
        // The same file through the app's own protocol, under the same token.
        let in_app = |range: Option<&str>| {
            let mut req = Request::builder().uri(format!("dioxus://index.html{}", in_app_path(&url)));
            if let Some(r) = range {
                req = req.header(header::RANGE, r);
            }
            in_app_response(&req.body(Vec::new()).unwrap())
        };
        let r = in_app(Some("bytes=2-4"));
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.headers()[header::CONTENT_RANGE], "bytes 2-4/10");
        assert_eq!(&r.body()[..], b"234");
        assert_eq!(&in_app(None).body()[..], b"0123456789");

        unpublish(&url);
        assert!(get(&url, None).starts_with("HTTP/1.1 404"));
        assert_eq!(in_app(None).status(), StatusCode::NOT_FOUND);
    }
}
