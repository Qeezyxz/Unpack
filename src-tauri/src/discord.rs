use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Discord application ID for Unpack Rich Presence.
/// The portal public key is only for HTTP interaction signatures and is not used here.
pub const APP_ID: &str = "1562281261504929922";

const REPO_URL: &str = "https://github.com/Qeezyxz/Unpack";

enum Cmd {
    Update { details: String, state: String },
    Enable(bool),
    Shutdown,
}

#[derive(Clone)]
pub struct PresenceHandle {
    tx: Sender<Cmd>,
}

impl PresenceHandle {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("unpack-discord".into())
            .spawn(move || worker(rx));
        // Starts disabled (R15 L1): nothing touches the Discord pipe until the page has loaded the
        // saved setting and sent `discord_set_enabled(true)`.
        PresenceHandle { tx: worker_tx }
    }

    pub fn update(&self, details: &str, state: &str) {
        let _ = self.tx.send(Cmd::Update {
            details: clip(details, 128),
            state: clip(state, 128),
        });
    }

    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.tx.send(Cmd::Enable(enabled));
    }
}

impl Drop for PresenceHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityText {
    pub details: String,
    pub state: String,
}

/// Map the current UI view / job onto Discord details + state lines.
pub fn activity_for(view: &str, job: Option<&str>) -> ActivityText {
    let details = match view {
        "extract" => "Extracting archives",
        "convert" => "Converting Xbox ISOs",
        "library" => "Browsing the game library",
        "usb" => "Copying to USB",
        "content" => "Placing DLC / title updates",
        "mods" => "Managing mod menus",
        "downloads" => "Downloading files",
        "homebrew" => "Browsing homebrew tools",
        "xbox" => "Sending to Xbox",
        "settings" => "Settings",
        _ => "Xbox 360 toolkit",
    };
    let state = match job.map(str::trim).filter(|s| !s.is_empty()) {
        Some(job) => job,
        None => "Idle",
    };
    ActivityText {
        details: details.into(),
        state: state.into(),
    }
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn worker(rx: Receiver<Cmd>) {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut enabled = false;
    let mut client: Option<DiscordIpcClient> = None;
    let mut last = ActivityText {
        details: "Xbox 360 toolkit".into(),
        state: "Idle".into(),
    };

    loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Cmd::Shutdown) => {
                if let Some(mut c) = client.take() {
                    let _ = c.clear_activity();
                    let _ = c.close();
                }
                break;
            }
            Ok(Cmd::Enable(on)) => {
                enabled = on;
                if !enabled {
                    if let Some(mut c) = client.take() {
                        let _ = c.clear_activity();
                        let _ = c.close();
                    }
                } else if client.is_none() {
                    client = connect_and_push(&last, started);
                }
            }
            Ok(Cmd::Update { details, state }) => {
                last = ActivityText { details, state };
                if enabled {
                    if client.is_none() {
                        client = connect_and_push(&last, started);
                    } else if let Some(c) = client.as_mut() {
                        if push_activity(c, &last, started).is_err() {
                            let _ = c.close();
                            client = connect_and_push(&last, started);
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if enabled && client.is_none() {
                    client = connect_and_push(&last, started);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn connect_and_push(last: &ActivityText, started: i64) -> Option<DiscordIpcClient> {
    let mut client = DiscordIpcClient::new(APP_ID).ok()?;
    client.connect().ok()?;
    if push_activity(&mut client, last, started).is_err() {
        let _ = client.close();
        return None;
    }
    Some(client)
}

fn push_activity(
    client: &mut DiscordIpcClient,
    last: &ActivityText,
    started: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let activity = activity::Activity::new()
        .details(last.details.as_str())
        .state(last.state.as_str())
        .activity_type(activity::ActivityType::Playing)
        .timestamps(activity::Timestamps::new().start(started))
        .assets(
            activity::Assets::new()
                .large_image("unpack")
                .large_text("Unpack"),
        )
        .buttons(vec![activity::Button::new("GitHub", REPO_URL)]);
    client.set_activity(activity)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_views_and_jobs() {
        let idle = activity_for("convert", None);
        assert_eq!(idle.details, "Converting Xbox ISOs");
        assert_eq!(idle.state, "Idle");
        let busy = activity_for("xbox", Some("Sending to Xbox…"));
        assert_eq!(busy.details, "Sending to Xbox");
        assert_eq!(busy.state, "Sending to Xbox…");
        let unknown = activity_for("nope", Some("  "));
        assert_eq!(unknown.details, "Xbox 360 toolkit");
        assert_eq!(unknown.state, "Idle");
    }

    #[test]
    fn application_id_is_the_unpack_app() {
        assert_eq!(APP_ID, "1562281261504929922");
        assert_eq!(APP_ID.len(), 19);
    }

    /// R16 L3: with a fake Discord IPC socket, nothing connects while disabled; once enabled the
    /// worker connects, hand-shakes with the Unpack app id, and sends SET_ACTIVITY with the text.
    #[cfg(unix)]
    #[test]
    fn r16_l3_connects_to_mock_ipc_once_enabled() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        let d = crate::testutil::TmpDir::new("discord-ipc");
        std::env::set_var("XDG_RUNTIME_DIR", d.join(""));
        let l = UnixListener::bind(d.join("discord-ipc-0")).unwrap();
        l.set_nonblocking(true).unwrap();
        let p = PresenceHandle::start();
        p.update("Settings", "Idle");
        std::thread::sleep(Duration::from_millis(500));
        assert!(l.accept().is_err(), "no connection while disabled");
        p.set_enabled(true);
        let t0 = std::time::Instant::now();
        let (mut s, _) = loop {
            match l.accept() {
                Ok(c) => break c,
                Err(_) if t0.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("never connected: {e}"),
            }
        };
        s.set_nonblocking(false).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let frame = |s: &mut std::os::unix::net::UnixStream| -> (u32, serde_json::Value) {
            let mut h = [0u8; 8];
            s.read_exact(&mut h).unwrap();
            let op = u32::from_le_bytes(h[..4].try_into().unwrap());
            let mut b = vec![0u8; u32::from_le_bytes(h[4..].try_into().unwrap()) as usize];
            s.read_exact(&mut b).unwrap();
            let reply = br#"{"cmd":"DISPATCH","evt":"READY","data":{}}"#;
            let mut out = 1u32.to_le_bytes().to_vec();
            out.extend((reply.len() as u32).to_le_bytes());
            out.extend(reply);
            s.write_all(&out).unwrap();
            (op, serde_json::from_slice(&b).unwrap())
        };
        let (op, hs) = frame(&mut s);
        assert_eq!((op, hs["client_id"].as_str()), (0, Some(APP_ID)), "{hs}");
        let (op, act) = frame(&mut s);
        assert_eq!(op, 1);
        assert_eq!(act["cmd"], "SET_ACTIVITY", "{act}");
        assert_eq!(act["args"]["activity"]["details"], "Settings", "{act}");
        assert_eq!(act["args"]["activity"]["state"], "Idle", "{act}");
        drop(p);
    }
}
