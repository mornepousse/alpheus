//! niri, READ ONLY: a connection to `$NIRI_SOCKET`, the `"EventStream"`
//! request, then the event stream until it closes. No other request.
//! Blocking read with no delay: no wakeup for nothing.
//!
//! Never:
//! - send a second request: `REQUEST` is the only text, pinned by
//!   `compositor_requests_are_read_only`;
//! - poll or wake at rest: the read blocks until niri writes.
//!
//! See: docs/compositors.md ("niri"), docs/features/privacy.md (niri's cast events, which this
//! stream carries).

use crate::compositor::CompositorState;
use crate::compositor::niri::Model;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// The only request sent to niri (checked by tests/guardrails.rs).
pub const REQUEST: &str = "\"EventStream\"";

pub fn session(socket: &Path, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()> {
    let mut s = UnixStream::connect(socket)?;
    s.write_all(REQUEST.as_bytes())?;
    s.write_all(b"\n")?;
    let mut reader = BufReader::new(s);
    let mut model = Model::default();
    let mut line = Vec::new();
    // The stream starts with the full state, WorkspacesChanged and
    // WindowsChanged: nothing is published before both (no half-read
    // state), so the first publication is the initial state and the next
    // ones are events (the supervisor counts on this). Any other event also
    // unblocks it, in case niri changes this order someday.
    let (mut workspaces_read, mut windows_read) = (false, false);
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "niri event stream closed"));
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        match model.apply_line(text) {
            Ok(true) => {
                if text.starts_with("{\"WorkspacesChanged\"") {
                    workspaces_read = true;
                } else if text.starts_with("{\"WindowsChanged\"") {
                    windows_read = true;
                } else {
                    (workspaces_read, windows_read) = (true, true);
                }
                if !(workspaces_read && windows_read) {
                    continue;
                }
                if !publish(model.state()) {
                    return Ok(());
                }
            }
            Ok(false) => {}
            Err(m) => return Err(io::Error::other(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    const STREAM: &str = include_str!("../../tests/fixtures/niri-evenements.jsonl");

    #[test]
    fn session_reads_stream_until_closed() {
        let directory = std::env::temp_dir().join(format!("alpheus-niri-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("niri.sock");
        let l = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut reader = BufReader::new(c.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut c = c;
            c.write_all(STREAM.as_bytes()).unwrap(); // then close
            request
        });
        let (tx, rx) = mpsc::channel();
        let r = session(&path, &mut |e| tx.send(e).is_ok());
        assert_eq!(server.join().unwrap().trim_end(), REQUEST);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let last = rx.try_iter().last().expect("at least one state published");
        assert_eq!(last.window.unwrap().class, "com.mitchellh.ghostty");
        assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn request_is_the_json_string_eventstream() {
        // niri expects a JSON request: the string "EventStream" in quotes.
        assert_eq!(serde_json::from_str::<String>(REQUEST).unwrap(), "EventStream");
    }

    #[test]
    fn niri_absent_error() {
        assert!(session(Path::new("/nonexistent/niri.sock"), &mut |_| true).is_err());
    }
}
