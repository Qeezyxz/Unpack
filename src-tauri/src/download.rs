use crate::fileops;
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
    // A hash typo should fail now, not after a multi-gigabyte download.
    if let Some(expected) = expected_hash {
        hashutil::normalize_hash(expected)?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Everything is written beside the final name and only renamed into place
    // once it is complete, so an interrupted download never looks finished and
    // can be continued from where it stopped.
    let tmp = fileops::sibling(dest, "unpack-part");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .build();
    let mut existing = tmp.metadata().map(|m| m.len()).unwrap_or(0);
    progress(format!("Connecting to {url}"), existing, existing);
    let response = loop {
        let mut request = agent.get(url);
        if existing > 0 {
            request = request.set("Range", &format!("bytes={existing}-"));
        }
        match request.call() {
            Ok(response) => break response,
            Err(ureq::Error::Status(416, _)) if existing > 0 => {
                // The saved part is not a prefix of what the server has now.
                let _ = std::fs::remove_file(&tmp);
                existing = 0;
            }
            Err(e) => return Err(format!("download failed: {e}")),
        }
    };
    let status = response.status();
    let resumed = status == 206 && existing > 0;
    if resumed && !range_starts_at(response.header("Content-Range"), existing) {
        let _ = std::fs::remove_file(&tmp);
        return Err("the server sent a different part of the file than the one already saved, so the partial download was discarded. Try again.".into());
    }
    let total = content_length(&response).map(|n| if resumed { existing + n } else { n });
    let mut reader = response.into_reader();
    let (mut file, mut written) = if resumed {
        (
            OpenOptions::new()
                .append(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?,
            existing,
        )
    } else {
        (File::create(&tmp).map_err(|e| e.to_string())?, 0)
    };
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            format!("download interrupted after {written} bytes (run it again to continue): {e}")
        })?;
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
    if let Some(total) = total {
        if written != total {
            return Err(format!(
                "download ended early ({written} of {total} bytes). Run it again to continue."
            ));
        }
    }
    let hash = if let Some(expected) = expected_hash.filter(|s| !s.trim().is_empty()) {
        match hashutil::verify_hash(&tmp, expected) {
            Ok(h) => Some(h),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    } else {
        None
    };
    fileops::replace_file(&tmp, dest)?;
    Ok(DownloadReport {
        url: url.to_string(),
        path: dest.display().to_string(),
        bytes: written,
        resumed,
        hash,
    })
}

fn range_starts_at(header: Option<&str>, offset: u64) -> bool {
    header
        .and_then(|h| h.trim().strip_prefix("bytes "))
        .and_then(|h| h.split('-').next())
        .and_then(|start| start.trim().parse::<u64>().ok())
        == Some(offset)
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
        .map(|s| safe_file_name(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download.bin".into())
}

/// The last part of a URL becomes a file name, so keep it to one plain name
/// that Windows can store.
fn safe_file_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    let short: String = trimmed.chars().take(120).collect();
    let short = short.trim_end_matches(['.', ' ']);
    if crate::paths::is_windows_device_name(short) {
        format!("_{short}")
    } else {
        short.to_string()
    }
}

pub fn dest_for_url(dir: &Path, url: &str) -> PathBuf {
    dir.join(default_download_name(url))
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_names_become_plain_file_names() {
        assert_eq!(super::default_download_name("https://h/a/Halo%203.zip"), "Halo%203.zip");
        assert_eq!(super::default_download_name("https://h/a/evil.txt:payload.exe"), "evil.txt_payload.exe");
        assert_eq!(super::default_download_name("https://h/a/name.."), "name");
        assert_eq!(super::default_download_name("https://h/"), "download.bin");
        assert_eq!(super::default_download_name("https://h/CON"), "_CON");
        assert_eq!(super::default_download_name("https://h/nul.txt"), "_nul.txt");
        assert_eq!(super::default_download_name(&format!("https://h/{}. x", "a".repeat(119))), "a".repeat(119));
        assert_eq!(super::default_download_name("https://h/%2e%2e"), "download.bin");
        assert!(super::default_download_name(&format!("https://h/{}", "a".repeat(400))).len() <= 120);
    }

    #[test]
    fn resume_needs_the_matching_range() {
        assert!(super::range_starts_at(Some("bytes 100-999/1000"), 100));
        assert!(!super::range_starts_at(Some("bytes 0-999/1000"), 100));
        assert!(!super::range_starts_at(Some("items 100-999/1000"), 100));
        assert!(!super::range_starts_at(None, 100));
    }

    use super::*;
    use std::io::Cursor;
    use tiny_http::{Header, Response, Server, StatusCode};

    #[derive(Clone, Copy)]
    enum Mode {
        Honor,
        IgnoreRange,
        Unsatisfiable,
        WrongRange,
    }

    fn header(name: &str, value: String) -> Header {
        Header::from_bytes(name.as_bytes(), value).unwrap()
    }

    /// Answers `requests` requests, then returns the Range header of each.
    fn serve(
        body: Vec<u8>,
        mode: Mode,
        requests: usize,
    ) -> (String, std::thread::JoinHandle<Vec<Option<String>>>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_string();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..requests {
                let request = server.recv().unwrap();
                let range = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Range"))
                    .map(|h| h.value.as_str().to_string());
                seen.push(range.clone());
                let start: usize = range
                    .as_deref()
                    .and_then(|r| r.trim_start_matches("bytes=").trim_end_matches('-').parse().ok())
                    .unwrap_or(0);
                match (mode, range.is_some()) {
                    (Mode::Unsatisfiable, true) => {
                        request
                            .respond(Response::empty(StatusCode(416)))
                            .unwrap();
                    }
                    (Mode::Honor | Mode::WrongRange, true) => {
                        let first = if matches!(mode, Mode::WrongRange) { 0 } else { start };
                        let slice = body[first..].to_vec();
                        let len = slice.len();
                        request
                            .respond(Response::new(
                                StatusCode(206),
                                vec![
                                    header("Content-Length", len.to_string()),
                                    header(
                                        "Content-Range",
                                        format!("bytes {first}-{}/{}", body.len() - 1, body.len()),
                                    ),
                                ],
                                Cursor::new(slice),
                                Some(len),
                                None,
                            ))
                            .unwrap();
                    }
                    _ => {
                        request
                            .respond(Response::from_data(body.clone()).with_status_code(200))
                            .unwrap();
                    }
                }
            }
            seen
        });
        (addr, handle)
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unpack-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn payload() -> Vec<u8> {
        (0..20_000u32).map(|x| (x % 251) as u8).collect()
    }

    #[test]
    fn resumes_partial_download() {
        let payload = payload();
        let (addr, handle) = serve(payload.clone(), Mode::Honor, 1);
        let dir = scratch("resume");
        let dest = dir.join("blob.bin");
        let part = fileops::sibling(&dest, "unpack-part");
        std::fs::write(&part, &payload[..8000]).unwrap();
        let report = download_url(&format!("http://{addr}/blob.bin"), &dest, None, |_, _, _| {}).unwrap();
        assert!(report.resumed);
        assert_eq!(report.bytes, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert!(!part.exists(), "the part file is renamed into place");
        assert_eq!(handle.join().unwrap(), vec![Some("bytes=8000-".to_string())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_finished_download_is_replaced_not_resumed() {
        let payload = payload();
        let (addr, handle) = serve(payload.clone(), Mode::Unsatisfiable, 1);
        let dir = scratch("again");
        let dest = dir.join("blob.bin");
        std::fs::write(&dest, b"older copy").unwrap();
        let report = download_url(&format!("http://{addr}/blob.bin"), &dest, None, |_, _, _| {}).unwrap();
        assert!(!report.resumed);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert_eq!(handle.join().unwrap(), vec![None]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_part_the_server_cannot_continue_starts_over() {
        let payload = payload();
        let (addr, handle) = serve(payload.clone(), Mode::Unsatisfiable, 2);
        let dir = scratch("416");
        let dest = dir.join("blob.bin");
        let part = fileops::sibling(&dest, "unpack-part");
        std::fs::write(&part, vec![5u8; payload.len()]).unwrap();
        let report = download_url(&format!("http://{addr}/blob.bin"), &dest, None, |_, _, _| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert!(!report.resumed);
        assert_eq!(handle.join().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_that_ignores_range_restarts_the_file() {
        let payload = payload();
        let (addr, handle) = serve(payload.clone(), Mode::IgnoreRange, 1);
        let dir = scratch("ignore");
        let dest = dir.join("blob.bin");
        std::fs::write(fileops::sibling(&dest, "unpack-part"), vec![1u8; 300]).unwrap();
        let report = download_url(&format!("http://{addr}/blob.bin"), &dest, None, |_, _, _| {}).unwrap();
        assert!(!report.resumed);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_hash_leaves_no_file() {
        let (addr, handle) = serve(payload(), Mode::IgnoreRange, 1);
        let dir = scratch("hash");
        let dest = dir.join("blob.bin");
        let err = download_url(
            &format!("http://{addr}/blob.bin"),
            &dest,
            Some(&"0".repeat(64)),
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(!err.is_empty());
        assert!(!dest.exists());
        assert!(!fileops::sibling(&dest, "unpack-part").exists());
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_append_the_wrong_part_of_a_file() {
        let (addr, handle) = serve(vec![9u8; 500], Mode::WrongRange, 1);
        let dir = scratch("wrong");
        let dest = dir.join("blob.bin");
        std::fs::write(fileops::sibling(&dest, "unpack-part"), vec![1u8; 100]).unwrap();
        let err = download_url(&format!("http://{addr}/blob.bin"), &dest, None, |_, _, _| {})
            .unwrap_err();
        assert!(err.contains("different part"), "{err}");
        assert!(!dest.exists(), "nothing may be saved under the final name");
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
