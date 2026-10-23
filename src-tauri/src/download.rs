use crate::hashutil;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Serialize)]
pub struct DownloadReport {
    pub url: String,
    pub path: String,
    pub bytes: u64,
    pub resumed: bool,
    pub hash: Option<String>,
}

pub fn download_url(
    url: &str,
    dest: &Path,
    expected_hash: Option<&str>,
    mut progress: impl FnMut(String, u64, u64),
) -> Result<DownloadReport, String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("only http and https URLs are allowed".into());
    }
    deny_non_loopback_if_testing(&parsed)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = dest.metadata().map(|m| m.len()).unwrap_or(0);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .build();
    let mut request = agent.get(url);
    if existing > 0 {
        request = request.set("Range", &format!("bytes={existing}-"));
    }
    progress(format!("Connecting to {url}"), existing, existing);
    let response = request.call().map_err(|e| format!("download failed: {e}"))?;
    let status = response.status();
    let resumed = status == 206 && existing > 0;
    if existing > 0 && status == 200 {
        // Server ignored Range; start over so the file is not concatenated.
        let _ = std::fs::remove_file(dest);
    }
    let total = content_length(&response).map(|n| if resumed { existing + n } else { n });
    let mut reader = response.into_reader();
    let mut file = if resumed {
        OpenOptions::new()
            .append(true)
            .open(dest)
            .map_err(|e| e.to_string())?
    } else {
        File::create(dest).map_err(|e| e.to_string())?
    };
    let mut buf = [0u8; 64 * 1024];
    let mut written = if resumed { existing } else { 0 };
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        written += n as u64;
        if let Some(total) = total {
            progress(format!("Downloading {written} / {total}"), written, total);
        } else {
            progress(format!("Downloading {written} bytes"), written, written);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    let hash = if let Some(expected) = expected_hash.filter(|s| !s.trim().is_empty()) {
        Some(hashutil::verify_hash(dest, expected)?)
    } else {
        None
    };
    Ok(DownloadReport {
        url: url.to_string(),
        path: dest.display().to_string(),
        bytes: written,
        resumed,
        hash,
    })
}

fn content_length(response: &ureq::Response) -> Option<u64> {
    response
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
}

fn deny_non_loopback_if_testing(url: &Url) -> Result<(), String> {
    #[cfg(feature = "download-test-loopback")]
    {
        match url.host_str() {
            Some("127.0.0.1") | Some("localhost") | Some("::1") => Ok(()),
            other => Err(format!(
                "download-test-loopback only allows localhost, got {other:?}"
            )),
        }
    }
    #[cfg(not(feature = "download-test-loopback"))]
    {
        let _ = url;
        Ok(())
    }
}

pub fn default_download_name(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download.bin".into())
}

pub fn dest_for_url(dir: &Path, url: &str) -> PathBuf {
    dir.join(default_download_name(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tiny_http::{Header, Response, Server, StatusCode};

    #[test]
    fn resumes_partial_download() {
        let payload = (0..20_000u32).map(|x| (x % 251) as u8).collect::<Vec<_>>();
        let server = Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_string();
        let body = payload.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv().unwrap();
                let range = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Range"))
                    .map(|h| h.value.as_str().to_string());
                if let Some(range) = range {
                    let start: usize = range
                        .trim_start_matches("bytes=")
                        .trim_end_matches('-')
                        .parse()
                        .unwrap();
                    let slice = body[start..].to_vec();
                    let len = slice.len();
                    request
                        .respond(
                            Response::new(
                                StatusCode(206),
                                vec![Header::from_bytes(&b"Content-Length"[..], len.to_string())
                                    .unwrap()],
                                Cursor::new(slice),
                                Some(len),
                                None,
                            ),
                        )
                        .unwrap();
                } else {
                    let slice = body[..8000].to_vec();
                    let len = slice.len();
                    request
                        .respond(Response::from_data(slice).with_status_code(200))
                        .unwrap();
                    let _ = len;
                }
            }
        });

        let dir = std::env::temp_dir().join(format!("unpack-dl-{}", std::process::id()));
        let dest = dir.join("blob.bin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("http://{addr}/blob.bin");
        let first = download_url(&url, &dest, None, |_, _, _| {}).unwrap();
        assert_eq!(first.bytes, 8000);
        assert!(!first.resumed);
        let second = download_url(&url, &dest, None, |_, _, _| {}).unwrap();
        assert!(second.resumed);
        assert_eq!(second.bytes, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_http() {
        let dest = std::env::temp_dir().join("unpack-dl-reject.bin");
        assert!(download_url("ftp://127.0.0.1/x", &dest, None, |_, _, _| {}).is_err());
        assert!(download_url("file:///etc/passwd", &dest, None, |_, _, _| {}).is_err());
    }
}
