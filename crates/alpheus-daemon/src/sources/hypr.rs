//! Hyprland, READ ONLY. Two sockets in
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! - `.socket2.sock`: the event stream, read continuously;
//! - `.socket.sock`: four read requests (`Request`), at startup and after a
//!   structure event. The stream never gives back the initial state, hence
//!   these reads. The only function that writes to this
//!   socket takes a `Request`, whose four texts are fixed and checked by
//!   tests/guardrails.rs: no action, no setting.
//!
//! Never:
//! - send anything but a `Request`: its four texts are fixed and pinned by
//!   `compositor_requests_are_read_only`, so no action and no setting can be sent from here;
//! - name the compositor's command-line client: its name is a forbidden word in every source of
//!   the daemon, which is why this line does not write it either;
//! - panic on what the stream says (`the_mango_and_sway_backends_have_no_panicking_path` covers
//!   its siblings; this file's own tests feed it malformed lines).
//!
//! See: docs/compositors.md ("Hyprland"), docs/features/bar.md, docs/decisions.md (the entries on
//! a compositor that fires a key bind twice).

use crate::compositor::CompositorState;
use crate::compositor::hypr::{Effect, Model, Responses, Snapshot, parse_line};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Monitors,
    Workspaces,
    Clients,
    ActiveWindow,
}

impl Request {
    pub const ALL: [Request; 4] = [Request::Monitors, Request::Workspaces, Request::Clients, Request::ActiveWindow];

    pub fn text(self) -> &'static str {
        match self {
            Request::Monitors => "j/monitors",
            Request::Workspaces => "j/workspaces",
            Request::Clients => "j/clients",
            Request::ActiveWindow => "j/activewindow",
        }
    }
}

/// Calm expected after a structure event before rereading the state: a burst
/// (ten windows closed at once) only costs one reread.
pub const CALM: Duration = Duration::from_millis(40);
/// Forced reread if the stream never calms down (a terminal's title updated
/// continuously).
pub const MAX_WAIT: Duration = Duration::from_millis(250);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Stream read delay: none as long as nothing is pending (the thread sleeps
/// without ever waking), `CALM` when a reread is due.
pub fn read_delay(due: bool) -> Option<Duration> {
    due.then_some(CALM)
}

fn request(directory: &Path, r: Request) -> io::Result<String> {
    let mut s = UnixStream::connect(directory.join(".socket.sock"))?;
    s.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    s.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    s.write_all(r.text().as_bytes())?;
    // Raw bytes then a tolerant conversion: a badly encoded window title
    // must not make the whole state unreadable.
    let mut rep = Vec::new();
    s.read_to_end(&mut rep)?;
    Ok(String::from_utf8_lossy(&rep).into_owned())
}

fn snapshot(directory: &Path) -> io::Result<Snapshot> {
    let m = request(directory, Request::Monitors)?;
    let w = request(directory, Request::Workspaces)?;
    let c = request(directory, Request::Clients)?;
    let a = request(directory, Request::ActiveWindow)?;
    Snapshot::parse(&Responses { monitors: &m, workspaces: &w, clients: &c, active: &a }).map_err(io::Error::other)
}

pub fn session(directory: &Path, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()> {
    // The stream first: an event that occurs while the state is being read
    // waits in the socket instead of being lost.
    let stream = UnixStream::connect(directory.join(".socket2.sock"))?;
    let mut model = Model::default();
    model.set_snapshot(snapshot(directory)?);
    if !publish(model.state()) {
        return Ok(());
    }
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut due: Option<Instant> = None; // reread due since
    loop {
        reader.get_ref().set_read_timeout(read_delay(due.is_some()))?;
        let reread = match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Hyprland event stream closed")),
            Ok(_) if line.ends_with(b"\n") => {
                let text = String::from_utf8_lossy(&line);
                let effect = model.apply(&parse_line(text.trim_end()));
                line.clear();
                match effect {
                    Effect::Nothing => {}
                    Effect::Changed => {
                        if !publish(model.state()) {
                            return Ok(());
                        }
                    }
                    Effect::Reread => {
                        due.get_or_insert_with(Instant::now);
                    }
                }
                due.is_some_and(|t| t.elapsed() >= MAX_WAIT)
            }
            Ok(_) => false, // incomplete line: the rest of the stream follows
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => due.is_some(),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
            Err(e) => return Err(e),
        };
        if reread {
            // A half-read line stays in `line` (read_until keeps the bytes
            // received): nothing is lost.
            model.set_snapshot(snapshot(directory)?);
            due = None;
            if !publish(model.state()) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Fake Hyprland: both sockets in a temporary directory, state served
    //! from the fixtures, requests received logged.
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub const MONITORS: &str = include_str!("../../tests/fixtures/hypr-monitors.json");
    pub const WORKSPACES: &str = include_str!("../../tests/fixtures/hypr-workspaces.json");
    pub const CLIENTS: &str = include_str!("../../tests/fixtures/hypr-clients.json");
    pub const ACTIVE: &str = include_str!("../../tests/fixtures/hypr-activewindow.json");

    pub struct FakeHyprland {
        pub directory: PathBuf,
        requests: Arc<Mutex<Vec<String>>>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    }

    impl FakeHyprland {
        pub fn start(name: &str) -> FakeHyprland {
            FakeHyprland::start_with(name, CLIENTS.as_bytes(), false)
        }

        /// `clients`: response served to `j/clients` (raw bytes); `flapper`:
        /// the stream accepts each connection then closes it right away.
        pub fn start_with(name: &str, clients: &[u8], flapper: bool) -> FakeHyprland {
            let clients = clients.to_vec();
            let directory = std::env::temp_dir().join(format!("alpheus-hypr-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stream = Arc::new(Mutex::new(None));
            let l1 = UnixListener::bind(directory.join(".socket.sock")).unwrap();
            let r = requests.clone();
            std::thread::spawn(move || {
                for c in l1.incoming() {
                    let Ok(mut c) = c else { return };
                    let mut buf = [0u8; 256];
                    let n = c.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let rep: &[u8] = match req.as_str() {
                        "j/monitors" => MONITORS.as_bytes(),
                        "j/workspaces" => WORKSPACES.as_bytes(),
                        "j/clients" => &clients,
                        "j/activewindow" => ACTIVE.as_bytes(),
                        _ => b"unknown request",
                    };
                    r.lock().unwrap().push(req);
                    let _ = c.write_all(rep);
                }
            });
            let l2 = UnixListener::bind(directory.join(".socket2.sock")).unwrap();
            let f = stream.clone();
            std::thread::spawn(move || {
                if flapper {
                    // Each connection closed as soon as it's accepted.
                    for c in l2.incoming() {
                        drop(c);
                    }
                } else if let Ok((c, _)) = l2.accept() {
                    *f.lock().unwrap() = Some(c);
                }
            });
            FakeHyprland { directory, requests, stream }
        }

        pub fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        /// Writes to the stream (waits for the connection, 5 s at most).
        pub fn write(&self, lines: &str) {
            let t = Instant::now();
            loop {
                if let Some(c) = self.stream.lock().unwrap().as_mut() {
                    c.write_all(lines.as_bytes()).unwrap();
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "stream never connected");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        pub fn close_stream(&self) {
            if let Some(c) = self.stream.lock().unwrap().take() {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    impl Drop for FakeHyprland {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeHyprland;
    use super::*;
    use std::sync::mpsc;

    const DELAY: Duration = Duration::from_secs(5);

    #[test]
    fn four_fixed_read_only_requests() {
        // Guardrail: only these four JSON reads go out on .socket.sock.
        let texts: Vec<&str> = Request::ALL.iter().map(|r| r.text()).collect();
        assert_eq!(texts, ["j/monitors", "j/workspaces", "j/clients", "j/activewindow"]);
    }

    #[test]
    fn no_delay_when_nothing_pending() {
        assert_eq!(read_delay(false), None, "no wakeup for nothing");
        assert_eq!(read_delay(true), Some(CALM));
    }

    #[test]
    fn session_publishes_state_then_follows_stream() {
        let f = FakeHyprland::start("follows");
        let (tx, rx) = mpsc::channel();
        let d = f.directory.clone();
        let h = std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        let e0 = rx.recv_timeout(DELAY).unwrap();
        assert_eq!(e0.workspaces.len(), 3);
        assert_eq!(f.requests().len(), 4);
        f.write("activewindow>>Alacritty,~/projets — zsh\n");
        assert_eq!(rx.recv_timeout(DELAY).unwrap().window.unwrap().class, "Alacritty");
        f.write("openwindow>>583d9c8a1b20,2,Alacritty,~\n");
        let _ = rx.recv_timeout(DELAY).unwrap(); // state reread
        assert_eq!(f.requests().len(), 8, "a full reread after a structure event");
        for r in f.requests() {
            assert!(Request::ALL.iter().any(|q| q.text() == r), "request outside the list: {r}");
        }
        f.close_stream();
        let end = h.join().unwrap();
        assert_eq!(end.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn reread_even_under_a_flood_of_titles() {
        let f = FakeHyprland::start("flood");
        let (tx, rx) = mpsc::channel();
        let d = f.directory.clone();
        std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        rx.recv_timeout(DELAY).unwrap();
        f.write("openwindow>>583d9c8a1b20,2,Alacritty,~\n");
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(600) {
            f.write("windowtitlev2>>583d9c8a1b20,compiling\n");
            std::thread::sleep(Duration::from_millis(10)); // never 40 ms of calm
        }
        assert!(f.requests().len() >= 8, "the reread must not wait for a calm that never comes");
    }

    #[test]
    fn invalid_utf8_title_does_not_break_state() {
        let clients = fake::CLIENTS.replacen("\"title\": \"~/projets", "\"title\": \"\u{1}~/projets", 1).into_bytes();
        // An invalid UTF-8 byte in place of the marker.
        let clients: Vec<u8> = clients.into_iter().map(|o| if o == 1 { 0xff } else { o }).collect();
        assert!(std::str::from_utf8(&clients).is_err());
        let f = FakeHyprland::start_with("utf8", &clients, false);
        let (tx, rx) = mpsc::channel();
        let d = f.directory.clone();
        std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        assert_eq!(rx.recv_timeout(DELAY).unwrap().workspaces.len(), 3);
    }

    #[test]
    fn hyprland_absent_error() {
        let r = session(Path::new("/nonexistent/hypr/x"), &mut |_| true);
        assert!(r.is_err());
    }
}
