//! Loopback HTTP bridge used to exercise resume downloads.
//!
//!   cargo run -p unpack --example bridge --features download-test-loopback

use std::io::Cursor;
use tiny_http::{Header, Response, Server, StatusCode};

const PAYLOAD: &[u8] = b"unpack-bridge-test-payload-0123456789";

fn main() {
    let server = Server::http("127.0.0.1:0").expect("bind loopback");
    eprintln!("UNPACK_BRIDGE=http://{}/file.bin", server.server_addr());
    for request in server.incoming_requests() {
        let range = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .map(|h| h.value.as_str().to_string());
        let (status, body) = if let Some(range) = range {
            let start: usize = range
                .trim_start_matches("bytes=")
                .split('-')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
                .min(PAYLOAD.len());
            (206, PAYLOAD[start..].to_vec())
        } else {
            (200, PAYLOAD.to_vec())
        };
        let len = body.len();
        let response = Response::new(
            StatusCode(status),
            vec![Header::from_bytes(&b"Content-Length"[..], len.to_string()).unwrap()],
            Cursor::new(body),
            Some(len),
            None,
        );
        let _ = request.respond(response);
    }
}
