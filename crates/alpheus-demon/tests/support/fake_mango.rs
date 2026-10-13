//! A fake Mango for the daemon's headless and memory tests: a socket in THIS test's runtime
//! directory (never the session's), the real 0.18 captures served with their outputs renamed after
//! headless sway's (HDMI-A-1 → HEADLESS-1, eDP-1 → HEADLESS-2), each watch connection kept open and
//! silent after its first snapshot (nothing to wake the daemon at rest).
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct FakeMango {
    pub socket: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    _held: Arc<Mutex<Vec<UnixStream>>>,
}

impl FakeMango {
    pub fn start(runtime: &Path) -> FakeMango {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let renamed = |name: &str| {
            let text = std::fs::read_to_string(fixtures.join(name)).unwrap();
            format!("{}\n", text.trim_end().replace("HDMI-A-1", "HEADLESS-1").replace("eDP-1", "HEADLESS-2"))
        };
        let (monitors, clients) = (renamed("mango-all-monitors.json"), renamed("mango-all-clients.json"));
        let socket = runtime.join("mango-fake.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (requests, held) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let (r, h) = (requests.clone(), held.clone());
        std::thread::spawn(move || {
            for c in listener.incoming() {
                let Ok(mut c) = c else { return };
                let mut line = String::new();
                if BufReader::new(&c).read_line(&mut line).is_err() {
                    continue;
                }
                // Recorded before the reply: once the daemon has its snapshot, the request is seen.
                r.lock().unwrap().push(line.trim_end().to_string());
                let reply = match line.as_str() {
                    "watch all-monitors\n" => monitors.as_str(),
                    "watch all-clients\n" => clients.as_str(),
                    _ => "{\"error\":\"unknown command\"}\n",
                };
                let _ = c.write_all(reply.as_bytes());
                h.lock().unwrap().push(c);
            }
        });
        FakeMango { socket, requests, _held: held }
    }

    /// The request lines received, without their newline, sorted.
    pub fn requests(&self) -> Vec<String> {
        let mut r = self.requests.lock().unwrap().clone();
        r.sort();
        r
    }
}
