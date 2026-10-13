//! Sway, READ ONLY, over the i3 IPC on `$SWAYSOCK`: one connection SUBSCRIBEd to the workspace and
//! window events, one for GET_WORKSPACES and GET_TREE, read at session start and once after each
//! burst of events. `Message` is the whole list of what is ever sent (checked by
//! tests/guardrails.rs): no command, no other request. The thread blocks on the event connection
//! with no timeout; a zero-timeout `poll` only drains a burst.

use crate::compositor::CompositorState;
use crate::compositor::sway::state;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub const MAGIC: &[u8; 6] = b"i3-ipc";
/// A message announcing more than this ends the session (spec § 3).
pub const MAX_REPLY: usize = 4 << 20;
/// The SUBSCRIBE payload: the two event kinds the bar follows.
pub const SUBSCRIPTION: &str = "[\"workspace\",\"window\"]";
/// The requests' connection only (spec § 3); the event connection has none.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Every message the daemon ever sends to sway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    GetWorkspaces,
    Subscribe,
    GetTree,
}

impl Message {
    pub const ALL: [Message; 3] = [Message::GetWorkspaces, Message::Subscribe, Message::GetTree];

    /// Its i3-ipc type.
    pub fn code(self) -> u32 {
        match self {
            Message::GetWorkspaces => 1,
            Message::Subscribe => 2,
            Message::GetTree => 4,
        }
    }

    /// Its payload: the subscription for SUBSCRIBE, nothing for the two reads.
    pub fn payload(self) -> &'static [u8] {
        match self {
            Message::Subscribe => SUBSCRIPTION.as_bytes(),
            Message::GetWorkspaces | Message::GetTree => b"",
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

fn snapshot(requests: &mut UnixStream) -> io::Result<CompositorState> {
    let workspaces = ask(requests, Message::GetWorkspaces)?;
    let tree = ask(requests, Message::GetTree)?;
    state(&workspaces, &tree).map_err(io::Error::other)
}

/// `{"success": true}`, and nothing else, is an accepted subscription.
fn subscribed(reply: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(reply).ok().and_then(|v| v.get("success").and_then(serde_json::Value::as_bool))
        == Some(true)
}

/// Readable right now (a zero-timeout poll): more of the same burst, read without waiting.
fn readable_now(s: &UnixStream) -> io::Result<bool> {
    let mut fd = libc::pollfd { fd: s.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: one live `pollfd`, a count of 1, a zero timeout.
        let r = unsafe { libc::poll(&mut fd, 1, 0) };
        if r >= 0 {
            return Ok(r > 0 && fd.revents != 0);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

pub fn session(socket: &Path, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()> {
    // The events first: one that happens while the state is read waits in the socket.
    let mut events = UnixStream::connect(socket)?;
    send(&mut events, Message::Subscribe)?;
    let (kind, reply) = read_frame(&mut events)?;
    if kind != Message::Subscribe.code() || !subscribed(&reply) {
        return Err(io::Error::other("sway refused the subscription"));
    }
    let mut requests = UnixStream::connect(socket)?;
    requests.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    requests.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut last = snapshot(&mut requests)?;
    if !publish(last.clone()) {
        return Ok(());
    }
    let mut events = BufReader::new(events);
    loop {
        // Blocks with no timeout: no wake-up at rest. The event's content is not read: any event means "read again".
        read_frame(&mut events)?;
        while !events.buffer().is_empty() || readable_now(events.get_ref())? {
            read_frame(&mut events)?;
        }
        let now = snapshot(&mut requests)?;
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

    /// A whole message as sway frames it.
    pub fn reply(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut m = b"i3-ipc".to_vec();
        m.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        m
    }

    /// What the fake answers: the bytes for a SUBSCRIBE and for a GET_TREE (GET_WORKSPACES always
    /// gets the fixture); `flapper`: the subscribed connection is closed right after its answer.
    pub struct Script {
        pub subscribe: Vec<u8>,
        pub tree: Vec<u8>,
        pub flapper: bool,
    }

    impl Default for Script {
        fn default() -> Self {
            Script { subscribe: reply(2, b"{\"success\": true}"), tree: reply(4, TREE), flapper: false }
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
                                _ => return,
                            };
                            if c.write_all(&answer).is_err() {
                                return;
                            }
                            if kind == 2 {
                                if script.flapper {
                                    return; // dropped: sway gone
                                }
                                *e.lock().unwrap() = c.try_clone().ok();
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
mod tests {
    use super::fake::{FakeSway, REFUSED, Script, TREE, WORKSPACES, reply};
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    const DELAY: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(300);

    /// A window event, as sway frames it (type 0x80000003).
    fn window_event() -> Vec<u8> {
        reply(0x8000_0003, br#"{"change":"title","container":{"id":4}}"#)
    }

    /// The states published and, apart, the session's own result. The session runs on a thread
    /// that reports through a channel: a mutant that never ends it fails on a timeout instead of
    /// hanging the run on a `join`.
    struct Run {
        states: mpsc::Receiver<CompositorState>,
        result: mpsc::Receiver<io::Result<()>>,
    }

    impl Run {
        fn start(f: &FakeSway, stack: Option<usize>) -> Run {
            let (tx, states) = mpsc::channel();
            let (done, result) = mpsc::channel();
            let socket = f.socket.clone();
            let work = move || {
                let r = session(&socket, &mut |e| tx.send(e).is_ok());
                let _ = done.send(r);
            };
            let builder = std::thread::Builder::new();
            let builder = if let Some(bytes) = stack { builder.stack_size(bytes) } else { builder };
            builder.spawn(work).unwrap();
            Run { states, result }
        }

        fn ended(&self) -> io::Error {
            self.result.recv_timeout(DELAY).expect("the session did not end").unwrap_err()
        }
    }

    fn run(f: &FakeSway) -> Run {
        Run::start(f, None)
    }

    /// The requests' connection's messages (types), once at least `n` arrived (5 s at most).
    fn reads(f: &FakeSway, n: usize) -> Vec<u32> {
        let t = Instant::now();
        loop {
            let r: Vec<u32> = f.received().into_iter().filter(|(c, _, _)| *c == 1).map(|(_, k, _)| k).collect();
            if r.len() >= n || t.elapsed() > DELAY {
                return r;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Spec § 3: the magic, a little-endian u32 length and type, the payload.
    #[test]
    fn frames_are_magic_length_type_payload() {
        assert_eq!(frame(Message::GetTree), b"i3-ipc\x00\x00\x00\x00\x04\x00\x00\x00");
        assert_eq!(frame(Message::GetWorkspaces), b"i3-ipc\x00\x00\x00\x00\x01\x00\x00\x00");
        let subscribe = frame(Message::Subscribe);
        assert_eq!(&subscribe[..14], b"i3-ipc\x16\x00\x00\x00\x02\x00\x00\x00", "22-byte payload, type 2");
        assert_eq!(&subscribe[14..], SUBSCRIPTION.as_bytes());
        assert_eq!(read_frame(&mut reply(4, b"{}").as_slice()).unwrap(), (4, b"{}".to_vec()));
        assert_eq!(read_frame(&mut &b"i3-ipc\x02\x00"[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "a cut header");
    }

    /// Spec § 3: SUBSCRIBE to the two events on one connection, then GET_WORKSPACES and GET_TREE
    /// on the other; the state is the model's for those replies.
    #[test]
    fn subscribe_first_then_the_two_reads() {
        let f = FakeSway::start("order", Script::default());
        let r = run(&f);
        let e = r.states.recv_timeout(DELAY).unwrap();
        assert_eq!(e, crate::compositor::sway::state(WORKSPACES, TREE).unwrap());
        let received = f.received();
        assert_eq!(received[0], (0, 2, SUBSCRIPTION.as_bytes().to_vec()));
        assert_eq!(received[1..], [(1, 1, Vec::new()), (1, 4, Vec::new())]);
    }

    /// Spec § 3: after an event, the events already readable are drained without blocking, then
    /// both requests are read once: a burst costs one re-read.
    #[test]
    fn a_burst_of_events_costs_one_reread() {
        let f = FakeSway::start("burst", Script::default());
        let r = run(&f);
        r.states.recv_timeout(DELAY).unwrap();
        f.push(&window_event().repeat(20));
        assert_eq!(reads(&f, 4), [1, 4, 1, 4]);
        std::thread::sleep(QUIET);
        assert_eq!(reads(&f, 4).len(), 4, "20 events, one re-read");
        f.push(&window_event());
        assert_eq!(reads(&f, 6), [1, 4, 1, 4, 1, 4], "a later event: one more");
        assert!(r.states.recv_timeout(QUIET).is_err(), "the same state: not published again (spec § 3)");
    }

    #[test]
    fn a_bad_magic_ends_the_session() {
        let f = FakeSway::start("magic", Script { subscribe: b"xx-ipc\x00\x00\x00\x00\x02\x00\x00\x00".to_vec(), ..Script::default() });
        let e = run(&f).ended();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("bad magic"), "{e}");
    }

    /// Spec § 3: a reply over 4 MiB ends the session before its payload is read (the fake never
    /// sends it: reading it would wait for the 2 s timeout instead).
    #[test]
    fn an_oversized_reply_ends_the_session() {
        let mut header = b"i3-ipc".to_vec();
        header.extend_from_slice(&((MAX_REPLY + 1) as u32).to_le_bytes());
        header.extend_from_slice(&4u32.to_le_bytes());
        let f = FakeSway::start("big", Script { tree: header, ..Script::default() });
        let t = Instant::now();
        let e = run(&f).ended();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        assert!(e.to_string().contains("over 4 MiB"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(1), "refused on the header, not after a timeout");
    }

    #[test]
    fn a_refused_subscription_ends_the_session() {
        let f = FakeSway::start("refused", Script { subscribe: reply(2, REFUSED), ..Script::default() });
        let e = run(&f).ended();
        assert!(e.to_string().contains("sway refused the subscription"), "{e}");
        assert!(f.received().iter().all(|(c, _, _)| *c == 0), "no read after a refused subscription");
    }

    #[test]
    fn eof_ends_the_session() {
        let f = FakeSway::start("eof", Script::default());
        let r = run(&f);
        r.states.recv_timeout(DELAY).unwrap();
        f.close_events();
        assert_eq!(r.ended().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sway_absent_error() {
        assert!(session(Path::new("/nonexistent/sway-ipc.sock"), &mut |_| true).is_err());
    }

    /// The tree's reply, nested `levels` deep in the shape that costs serde_json the most stack.
    fn nested(levels: usize) -> Vec<u8> {
        let mut objects = String::from("0");
        for _ in 0..levels {
            objects = format!(r#"{{"p0":0,"p1":"x","p2":null,"z":{objects}}}"#);
        }
        reply(4, objects.as_bytes())
    }

    /// Spec § 3, on the compositor thread's real 256 KiB stack (`spawn_source`'s): a tree nested
    /// exactly MAX_NESTING levels is read and published; one level more ends the session as
    /// unreadable, before serde_json recurses.
    #[test]
    fn a_deep_reply_is_bounded_on_the_compositor_stack() {
        let f = FakeSway::start("deep", Script { tree: nested(crate::compositor::sway::MAX_NESTING), ..Script::default() });
        let r = Run::start(&f, Some(256 * 1024));
        let e = r.states.recv_timeout(DELAY).expect("a tree at MAX_NESTING is read");
        assert!(e.toplevels.is_empty());

        let f = FakeSway::start("deeper", Script { tree: nested(crate::compositor::sway::MAX_NESTING + 1), ..Script::default() });
        let r = Run::start(&f, Some(256 * 1024));
        let e = r.ended();
        assert!(e.to_string().contains("unreadable sway tree"), "{e}");
        assert!(r.states.try_recv().is_err(), "nothing published");
    }
}
