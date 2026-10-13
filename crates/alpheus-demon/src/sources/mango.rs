//! Mango, READ ONLY: two connections to the socket `$MANGO_INSTANCE_SIGNATURE` names, one request
//! line each (`REQUESTS`, checked by tests/guardrails.rs), then the full snapshots Mango writes
//! on every change, one JSON line each. Nothing else is ever written. The thread blocks in one
//! `poll` on both connections, never with a timeout: no wake-up at rest. Mango's command-line
//! client is never run.

use crate::compositor::CompositorState;
use crate::compositor::mango::{Model, Watch};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// The only two lines ever sent to Mango, one per connection (each followed by a newline).
pub const REQUESTS: [&str; 2] = ["watch all-monitors", "watch all-clients"];
/// A line longer than this (its newline aside) ends the session (spec § 2).
pub const MAX_LINE: usize = 1 << 20;
/// One read at most; a snapshot is about 5 KB.
const CHUNK: usize = 64 * 1024;

fn request_line(watch: Watch) -> String {
    let [monitors, clients] = REQUESTS;
    let request = match watch {
        Watch::Monitors => monitors,
        Watch::Clients => clients,
    };
    format!("{request}\n")
}

fn too_long() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a mango line over 1 MiB")
}

/// One watch connection and the bytes read past its last complete line.
struct Stream {
    watch: Watch,
    socket: UnixStream,
    pending: Vec<u8>,
}

impl Stream {
    fn open(path: &Path, watch: Watch) -> io::Result<Stream> {
        let mut socket = UnixStream::connect(path)?;
        socket.write_all(request_line(watch).as_bytes())?;
        Ok(Stream { watch, socket, pending: Vec::new() })
    }

    /// One read: `poll` said readable, so it does not block. EOF ends the session.
    fn fill(&mut self, chunk: &mut [u8]) -> io::Result<()> {
        let n = loop {
            match self.socket.read(chunk) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => break r?,
            }
        };
        if n == 0 {
            let which = if self.watch == Watch::Monitors { "monitors" } else { "clients" };
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("mango closed the {which} watch")));
        }
        self.pending.extend_from_slice(chunk.get(..n).unwrap_or_default());
        Ok(())
    }

    /// The next complete line, its newline removed; None until one is complete. A line past
    /// MAX_LINE, complete or not, ends the session.
    fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self.pending.iter().position(|&b| b == b'\n') {
            Some(i) if i > MAX_LINE => Err(too_long()),
            Some(i) => {
                let mut line: Vec<u8> = self.pending.drain(..=i).collect();
                line.pop();
                if self.pending.is_empty() && self.pending.capacity() > 4 * CHUNK {
                    self.pending = Vec::new(); // a long line read: its buffer is not kept
                }
                Ok(Some(line))
            }
            None if self.pending.len() > MAX_LINE => Err(too_long()),
            None => Ok(None),
        }
    }
}

/// Blocks until one of the two connections is readable or hung up. No timeout: no wake-up at rest.
fn wait(streams: &[Stream; 2]) -> io::Result<[bool; 2]> {
    let mut fds = streams.each_ref().map(|s| libc::pollfd { fd: s.socket.as_raw_fd(), events: libc::POLLIN, revents: 0 });
    loop {
        // SAFETY: `fds` is a live array of two `pollfd` for the whole call, its length given with it.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if r >= 0 {
            return Ok(fds.map(|f| f.revents != 0));
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

pub fn session(socket: &Path, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()> {
    let mut streams = [Stream::open(socket, Watch::Monitors)?, Stream::open(socket, Watch::Clients)?];
    let mut model = Model::default();
    let mut chunk = vec![0u8; CHUNK];
    let mut last: Option<CompositorState> = None;
    loop {
        let readable = wait(&streams)?;
        for (stream, ready) in streams.iter_mut().zip(readable) {
            if !ready {
                continue;
            }
            stream.fill(&mut chunk)?;
            while let Some(line) = stream.next_line()? {
                // Lossy: a badly encoded title must not make the whole state unreadable.
                model.apply(stream.watch, &String::from_utf8_lossy(&line)).map_err(io::Error::other)?;
            }
        }
        // Nothing before both first snapshots (no half-read state); then only a state that
        // changed (spec § 2): Mango rewrites every client's geometry on a move.
        if !model.ready() {
            continue;
        }
        let state = model.state();
        if last.as_ref() == Some(&state) {
            continue;
        }
        if !publish(state.clone()) {
            return Ok(());
        }
        last = Some(state);
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Fake Mango: a socket in a temporary directory; each connection's request line recorded, the
    //! first snapshot the test gives written back at once, the connection kept for `push` and
    //! `close`. Never a real Mango socket.
    //!
    //! Kept in the crate (not in `tests/support`) because the unit tests of `session` and of
    //! `sources/mod.rs` cannot reach the integration tests' support code.
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub const MONITORS: &str = include_str!("../../tests/fixtures/mango-all-monitors.json");
    pub const CLIENTS: &str = include_str!("../../tests/fixtures/mango-all-clients.json");

    pub struct FakeMango {
        pub socket: PathBuf,
        directory: PathBuf,
        requests: Arc<Mutex<Vec<String>>>,
        streams: Arc<Mutex<Vec<(String, UnixStream)>>>,
    }

    impl FakeMango {
        /// `monitors`/`clients`: the line each watch writes as soon as its request arrives (a
        /// newline added); None: nothing until `push`.
        pub fn start(name: &str, monitors: Option<&str>, clients: Option<&str>) -> FakeMango {
            let directory = std::env::temp_dir().join(format!("alpheus-mango-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            let socket = directory.join("mango.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (requests, streams) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
            let (r, s) = (requests.clone(), streams.clone());
            let first = [monitors.map(|t| format!("{}\n", t.trim_end())), clients.map(|t| format!("{}\n", t.trim_end()))];
            std::thread::spawn(move || {
                for c in listener.incoming() {
                    let Ok(mut c) = c else { return };
                    let mut line = String::new();
                    if BufReader::new(&c).read_line(&mut line).is_err() {
                        continue;
                    }
                    let reply = match line.as_str() {
                        "watch all-monitors\n" => first[0].clone(),
                        "watch all-clients\n" => first[1].clone(),
                        _ => Some("{\"error\":\"unknown command\"}\n".to_string()),
                    };
                    // Recorded before the reply: once the state is read, the request is there.
                    r.lock().unwrap().push(line.clone());
                    if let Some(reply) = reply {
                        let _ = c.write_all(reply.as_bytes());
                    }
                    s.lock().unwrap().push((line.trim_end().to_string(), c));
                }
            });
            FakeMango { socket, directory, requests, streams }
        }

        /// The request lines received, newline included, in arrival order.
        pub fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        /// Writes `bytes` to the connection that sent `request` ("watch all-clients"), once it is
        /// there (5 s at most).
        pub fn push(&self, request: &str, bytes: &[u8]) {
            let t = Instant::now();
            loop {
                if let Some((_, c)) = self.streams.lock().unwrap().iter_mut().find(|(r, _)| r == request) {
                    c.write_all(bytes).unwrap();
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "{request} never connected");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Closes the connection that sent `request` (Mango going away).
        pub fn close(&self, request: &str) {
            let t = Instant::now();
            loop {
                if let Some((_, c)) = self.streams.lock().unwrap().iter().find(|(r, _)| r == request) {
                    let _ = c.shutdown(std::net::Shutdown::Both);
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "{request} never connected");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for FakeMango {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{CLIENTS, FakeMango, MONITORS};
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const DELAY: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(300);
    const ERROR: &str = include_str!("../../tests/fixtures/mango-error.json");

    /// The states published and, apart, the session's own result. The session reports through a
    /// channel: a mutant that never ends it fails on a timeout instead of hanging the run on a
    /// `join`.
    fn run(f: &FakeMango) -> (mpsc::Receiver<CompositorState>, Ended) {
        let (tx, rx) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let socket = f.socket.clone();
        std::thread::spawn(move || {
            let r = session(&socket, &mut |e| tx.send(e).is_ok());
            let _ = done.send(r);
        });
        (rx, Ended(result))
    }

    struct Ended(mpsc::Receiver<io::Result<()>>);

    impl Ended {
        /// The session's error, within DELAY.
        fn error(&self) -> io::Error {
            self.0.recv_timeout(DELAY).expect("the session did not end").unwrap_err()
        }
    }

    fn line(text: &str) -> Vec<u8> {
        format!("{}\n", text.trim_end()).into_bytes()
    }

    /// Spec § 2: two connections, one request line each, then the state.
    #[test]
    fn two_watches_one_request_line_each_then_the_state() {
        let f = FakeMango::start("two", Some(MONITORS), Some(CLIENTS));
        let (rx, _h) = run(&f);
        let e = rx.recv_timeout(DELAY).unwrap();
        assert_eq!(e.workspaces.len(), 2);
        assert_eq!(e.toplevels.len(), 4);
        let mut requests = f.requests();
        requests.sort();
        assert_eq!(requests, ["watch all-clients\n", "watch all-monitors\n"]);
        assert_eq!(REQUESTS, ["watch all-monitors", "watch all-clients"]);
    }

    /// Spec § 2: nothing published until both first snapshots have arrived.
    #[test]
    fn nothing_is_published_before_both_first_snapshots() {
        let f = FakeMango::start("half", Some(MONITORS), None);
        let (rx, _h) = run(&f);
        assert!(rx.recv_timeout(QUIET).is_err(), "monitors alone: nothing yet");
        f.push("watch all-clients", &line(CLIENTS));
        assert_eq!(rx.recv_timeout(DELAY).unwrap().toplevels.len(), 4);
    }

    /// Review Focus 1 (spec § 2): Mango rewrites every client's geometry on a move; nothing the
    /// bar shows changed, nothing is published.
    #[test]
    fn a_new_snapshot_is_published_only_when_the_state_changes() {
        let f = FakeMango::start("change", Some(MONITORS), Some(CLIENTS));
        let (rx, _h) = run(&f);
        rx.recv_timeout(DELAY).unwrap();
        let moved = CLIENTS.replace("\"x\": 2563", "\"x\": 2600");
        assert_ne!(moved, CLIENTS);
        f.push("watch all-clients", &line(&moved));
        assert!(rx.recv_timeout(QUIET).is_err(), "geometry only: no publication");
        let refocused =
            CLIENTS.replace("\"is_focused\": true", "\"is_focused\": false").replacen("\"is_focused\": false", "\"is_focused\": true", 1);
        f.push("watch all-clients", &line(&refocused));
        let e = rx.recv_timeout(DELAY).unwrap();
        assert_eq!(e.toplevels.last().map(|t| t.selector.as_str()), Some("id:28"), "28 (the first client) now focused, last");
    }

    #[test]
    fn a_snapshot_split_across_reads_and_two_in_one_read() {
        let f = FakeMango::start("split", Some(MONITORS), Some(CLIENTS));
        let (rx, _h) = run(&f);
        rx.recv_timeout(DELAY).unwrap();
        let refocused = line(&CLIENTS.replace("\"is_focused\": true", "\"is_focused\": false"));
        let (a, b) = refocused.split_at(refocused.len() / 2);
        f.push("watch all-clients", a);
        assert!(rx.recv_timeout(QUIET).is_err(), "half a line: nothing");
        let again = line(&CLIENTS.replace("\"is_focused\": true", "\"is_focused\": false").replacen(
            "\"is_focused\": false",
            "\"is_focused\": true",
            1,
        ));
        f.push("watch all-clients", &[b, again.as_slice()].concat());
        let e = rx.recv_timeout(DELAY).unwrap();
        assert_eq!(e.toplevels.last().map(|t| t.selector.as_str()), Some("id:28"), "the second line of the read wins");
        assert!(rx.recv_timeout(QUIET).is_err(), "one publication for the two lines");
    }

    #[test]
    fn eof_on_either_watch_ends_the_session() {
        for request in ["watch all-monitors", "watch all-clients"] {
            let f = FakeMango::start("eof", Some(MONITORS), Some(CLIENTS));
            let (rx, h) = run(&f);
            rx.recv_timeout(DELAY).unwrap();
            f.close(request);
            assert_eq!(h.error().kind(), io::ErrorKind::UnexpectedEof, "{request}");
        }
    }

    #[test]
    fn an_error_reply_ends_the_session() {
        let f = FakeMango::start("error", Some(ERROR), Some(CLIENTS));
        let (rx, h) = run(&f);
        let e = h.error();
        assert!(e.to_string().contains("mango refused"), "{e}");
        assert!(rx.try_recv().is_err(), "nothing published");
    }

    #[test]
    fn a_line_that_is_not_json_ends_the_session() {
        let f = FakeMango::start("garbage", Some(MONITORS), Some("Failed to connect"));
        let (_rx, h) = run(&f);
        assert!(h.error().to_string().contains("unreadable mango line"));
    }

    /// Spec § 2: a line of exactly 1 MiB is read; one byte more ends the session.
    #[test]
    fn a_line_over_1_mib_ends_the_session() {
        let f = FakeMango::start("long", Some(MONITORS), Some(CLIENTS));
        let (rx, h) = run(&f);
        rx.recv_timeout(DELAY).unwrap();
        let mut exact = b"{\"clients\":[]}".to_vec();
        exact.resize(MAX_LINE, b' ');
        exact.push(b'\n');
        f.push("watch all-clients", &exact);
        let e = rx.recv_timeout(DELAY).unwrap();
        assert!(e.toplevels.is_empty(), "the 1 MiB line was read");
        f.push("watch all-clients", &vec![b' '; MAX_LINE + 1]);
        let err = h.error();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert!(err.to_string().contains("over 1 MiB"), "{err}");
    }

    #[test]
    fn mango_absent_error() {
        assert!(session(Path::new("/nonexistent/mango.sock"), &mut |_| true).is_err());
    }
}
