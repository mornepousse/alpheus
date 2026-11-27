//! Sway, READ ONLY, over the i3 IPC on `$SWAYSOCK`: one connection SUBSCRIBEd to the workspace and
//! window events, one for GET_WORKSPACES and GET_TREE, read at session start and once after each
//! burst of workspace or window events. With the keyboard wanted, the subscription adds the
//! `input` and `mode` events and GET_INPUTS is read once per session; those events carry what
//! they change, so they never cost a read. `Message` is the whole list of what is ever sent
//! (checked by tests/guardrails.rs): no command, no other request. The thread blocks on the event
//! connection with no timeout; a zero-timeout `poll` only drains a burst.
//!
//! Never:
//! - send anything outside `Message`'s five messages (`sway_writes_only_its_messages`): the two
//!   subscriptions and three reads, GET_INPUTS being a read with no payload; never the i3 command
//!   message; Sway's client is a forbidden word in the daemon's sources too;
//! - poll with a timeout: the event connection blocks, and a zero-timeout `poll` only drains a
//!   burst that has already arrived;
//! - panic on a reply (`the_mango_and_sway_backends_have_no_panicking_path`).
//!
//! See: docs/compositors.md ("Sway"), docs/features/bar.md.

use crate::compositor::CompositorState;
use crate::compositor::sway::Model;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub const MAGIC: &[u8; 6] = b"i3-ipc";
/// A message announcing more than this ends the session.
pub const MAX_REPLY: usize = 4 << 20;
/// The SUBSCRIBE payload: the two event kinds the bar follows.
pub const SUBSCRIPTION: &str = "[\"workspace\",\"window\"]";
/// The SUBSCRIBE payload with the keyboard wanted: the layout and the bind mode too.
pub const SUBSCRIPTION_KEYBOARD: &str = "[\"workspace\",\"window\",\"input\",\"mode\"]";
/// The i3-ipc events' types (the high bit marks an event).
const WORKSPACE_EVENT: u32 = 0x8000_0000;
const MODE_EVENT: u32 = 0x8000_0002;
const WINDOW_EVENT: u32 = 0x8000_0003;
const INPUT_EVENT: u32 = 0x8000_0015;
/// The requests' connection only; the event connection has none.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Events drained in one go before the state is read again: an endless stream of events still
/// refreshes the bar.
const MAX_BURST: usize = 256;

/// Every message the daemon ever sends to sway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    GetWorkspaces,
    Subscribe,
    /// SUBSCRIBE with the `input` and `mode` events, sent in place of `Subscribe` when the
    /// keyboard is wanted.
    SubscribeKeyboard,
    GetTree,
    /// A read: the input devices, each keyboard's active layout among them.
    GetInputs,
}

impl Message {
    pub const ALL: [Message; 5] =
        [Message::GetWorkspaces, Message::Subscribe, Message::SubscribeKeyboard, Message::GetTree, Message::GetInputs];

    /// Its i3-ipc type.
    pub fn code(self) -> u32 {
        match self {
            Message::GetWorkspaces => 1,
            Message::Subscribe | Message::SubscribeKeyboard => 2,
            Message::GetTree => 4,
            Message::GetInputs => 100,
        }
    }

    /// Its payload: the subscription for a SUBSCRIBE, nothing for the three reads.
    pub fn payload(self) -> &'static [u8] {
        match self {
            Message::Subscribe => SUBSCRIPTION.as_bytes(),
            Message::SubscribeKeyboard => SUBSCRIPTION_KEYBOARD.as_bytes(),
            Message::GetWorkspaces | Message::GetTree | Message::GetInputs => b"",
        }
    }
}

/// The whole message `m`: the magic, the payload's length and the type (little-endian u32), the payload.
pub fn frame(m: Message) -> Vec<u8> {
    let payload = m.payload();
    let mut out = Vec::with_capacity(14 + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&m.code().to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn invalid(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

/// One message: (type, payload). A bad magic, or a length over MAX_REPLY, ends the session before
/// anything is allocated for the payload.
pub fn read_frame(r: &mut impl Read) -> io::Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 14];
    r.read_exact(&mut header)?;
    let [m0, m1, m2, m3, m4, m5, l0, l1, l2, l3, t0, t1, t2, t3] = header;
    if &[m0, m1, m2, m3, m4, m5] != MAGIC {
        return Err(invalid("sway: bad magic".to_string()));
    }
    let len = u32::from_le_bytes([l0, l1, l2, l3]) as usize;
    if len > MAX_REPLY {
        return Err(invalid(format!("sway: a {len}-byte message, over 4 MiB")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((u32::from_le_bytes([t0, t1, t2, t3]), payload))
}

/// The one write of this source: a `Message`'s frame, nothing else.
fn send(s: &mut UnixStream, m: Message) -> io::Result<()> {
    s.write_all(&frame(m))
}

/// `m` sent, its reply read; a reply of another type ends the session.
fn ask(s: &mut UnixStream, m: Message) -> io::Result<Vec<u8>> {
    send(s, m)?;
    let (kind, payload) = read_frame(s)?;
    if kind != m.code() {
        return Err(invalid(format!("sway answered type {kind} to type {}", m.code())));
    }
    Ok(payload)
}

fn snapshot(requests: &mut UnixStream, model: &mut Model) -> io::Result<CompositorState> {
    let workspaces = ask(requests, Message::GetWorkspaces)?;
    let tree = ask(requests, Message::GetTree)?;
    model.state(&workspaces, &tree).map_err(io::Error::other)
}

/// `{"success": true}`, and nothing else, is an accepted subscription.
fn subscribed(reply: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(reply).ok().and_then(|v| v.get("success").and_then(serde_json::Value::as_bool))
        == Some(true)
}

/// Readable right now (a zero-timeout poll): more of the same burst, read without waiting.
fn readable_now(s: &UnixStream) -> io::Result<bool> {
    let mut fds = [libc::pollfd { fd: s.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    let ready = crate::sys::poll(&mut fds, 0)?;
    let [fd] = fds;
    Ok(ready > 0 && fd.revents != 0)
}

/// One event into the model; true: the tree must be read again. A window event also goes to
/// the focus history; a workspace event (whole workspaces, never parsed) is only that cue. An
/// `input` or `mode` event carries what it changes: into the model, no read.
// The workspace event spelled out beside the arm for the events never subscribed to.
#[allow(clippy::match_same_arms)]
fn follow(model: &mut Model, (kind, payload): (u32, Vec<u8>)) -> bool {
    match kind {
        WINDOW_EVENT => {
            model.event(&payload);
            true
        }
        INPUT_EVENT => {
            model.input_event(&payload);
            false
        }
        MODE_EVENT => {
            model.mode_event(&payload);
            false
        }
        WORKSPACE_EVENT => true,
        // Never subscribed to: read again, as for any change.
        _ => true,
    }
}

/// `keyboard`: subscribe to the `input` and `mode` events too, and read GET_INPUTS once, at the
/// session's start; the states carry the layout and the mode.
pub fn session(socket: &Path, keyboard: bool, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()> {
    // The events first: one that happens while the state is read waits in the socket.
    let subscribe = if keyboard { Message::SubscribeKeyboard } else { Message::Subscribe };
    let mut events = UnixStream::connect(socket)?;
    send(&mut events, subscribe)?;
    let (kind, reply) = read_frame(&mut events)?;
    if kind != subscribe.code() || !subscribed(&reply) {
        return Err(io::Error::other("sway refused the subscription"));
    }
    let mut requests = UnixStream::connect(socket)?;
    requests.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    requests.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    // The focus history and the keyboard live as long as the session: a new session starts from
    // the tree and GET_INPUTS. Then the `input` events alone follow the layout.
    let mut model = Model::keyboard(keyboard);
    if keyboard {
        model.inputs(&ask(&mut requests, Message::GetInputs)?);
    }
    let mut last = snapshot(&mut requests, &mut model)?;
    if !publish(last.clone()) {
        return Ok(());
    }
    let mut events = BufReader::new(events);
    loop {
        // Blocks with no timeout: no wake-up at rest. A workspace or window event means "read
        // again" (a window event's focus change also feeds the focus history); a burst of
        // `input` and `mode` events alone reads nothing.
        let mut reread = follow(&mut model, read_frame(&mut events)?);
        let mut drained = 1;
        while drained < MAX_BURST && (!events.buffer().is_empty() || readable_now(events.get_ref())?) {
            reread |= follow(&mut model, read_frame(&mut events)?);
            drained += 1;
        }
        let now = if reread { snapshot(&mut requests, &mut model)? } else { model.with_keyboard(&last) };
        if now != last {
            if !publish(now.clone()) {
                return Ok(());
            }
            last = now;
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Fake sway: an i3-ipc socket in a temporary directory. Every message received is recorded
    //! with its connection's number; the answers are scripted (`Script`); the subscribed connection
    //! is kept for `push` (events) and `close_events`. Never a real sway socket.
    use super::read_frame;
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub const WORKSPACES: &[u8] = include_bytes!("../../tests/fixtures/sway-workspaces.json");
    pub const TREE: &[u8] = include_bytes!("../../tests/fixtures/sway-tree.json");
    pub const REFUSED: &[u8] = include_bytes!("../../tests/fixtures/sway-subscribe-refused.json");
    pub const INPUTS: &[u8] = include_bytes!("../../tests/fixtures/sway-inputs.json");

    /// A whole message as sway frames it.
    pub fn reply(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut m = b"i3-ipc".to_vec();
        m.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        m
    }

    /// What the fake answers: the bytes for a SUBSCRIBE, a GET_TREE and a GET_INPUTS
    /// (GET_WORKSPACES always gets the fixture); `flapper`: the subscribed connection is closed
    /// right after its answer.
    pub struct Script {
        pub subscribe: Vec<u8>,
        pub tree: Vec<u8>,
        pub inputs: Vec<u8>,
        pub flapper: bool,
    }

    impl Default for Script {
        fn default() -> Self {
            Script { subscribe: reply(2, b"{\"success\": true}"), tree: reply(4, TREE), inputs: reply(100, INPUTS), flapper: false }
        }
    }

    /// (connection number, type, payload) of a message received.
    pub type Received = (usize, u32, Vec<u8>);

    pub struct FakeSway {
        pub socket: PathBuf,
        directory: PathBuf,
        received: Arc<Mutex<Vec<Received>>>,
        events: Arc<Mutex<Option<UnixStream>>>,
    }

    impl FakeSway {
        pub fn start(name: &str, script: Script) -> FakeSway {
            let directory = std::env::temp_dir().join(format!("alpheus-sway-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            let socket = directory.join("sway-ipc.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (received, events) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(None)));
            let (r, e) = (received.clone(), events.clone());
            let script = Arc::new(script);
            let count = Arc::new(AtomicUsize::new(0));
            std::thread::spawn(move || {
                for c in listener.incoming() {
                    let Ok(mut c) = c else { return };
                    let (r, e, script) = (r.clone(), e.clone(), script.clone());
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    std::thread::spawn(move || {
                        while let Ok((kind, payload)) = read_frame(&mut c) {
                            r.lock().unwrap().push((n, kind, payload));
                            let answer = match kind {
                                1 => reply(1, WORKSPACES),
                                2 => script.subscribe.clone(),
                                4 => script.tree.clone(),
                                100 => script.inputs.clone(),
                                _ => return,
                            };
                            // The subscribed connection is kept before its answer goes out: once
                            // answered, the session may publish and the test close it at once.
                            if kind == 2 && !script.flapper {
                                *e.lock().unwrap() = c.try_clone().ok();
                            }
                            if c.write_all(&answer).is_err() {
                                return;
                            }
                            if kind == 2 && script.flapper {
                                return; // dropped: sway gone
                            }
                        }
                    });
                }
            });
            FakeSway { socket, directory, received, events }
        }

        /// (connection number, type, payload) of every message received, in order.
        pub fn received(&self) -> Vec<Received> {
            self.received.lock().unwrap().clone()
        }

        /// Writes `bytes` (whole events) on the subscribed connection, once it is there (5 s at most).
        pub fn push(&self, bytes: &[u8]) {
            let t = Instant::now();
            loop {
                if let Some(c) = self.events.lock().unwrap().as_mut() {
                    c.write_all(bytes).unwrap();
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "never subscribed");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        pub fn close_events(&self) {
            if let Some(c) = self.events.lock().unwrap().take() {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    impl Drop for FakeSway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests;
