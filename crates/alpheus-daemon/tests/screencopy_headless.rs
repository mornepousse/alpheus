//! Screencopy against a real compositor: one capture of a disposable headless sway, with the
//! daemon's own `Screencopy` and `encode_png`. Requires headless sway (`nix develop`):
//! `cargo test -p alpheus-daemon --test screencopy_headless -- --ignored`.
//! Never the session's compositor: the client connects to the harness's own socket.

mod common;

use alpheus_daemon::captures::Rect;
use alpheus_daemon::captures::encode::encode_png;
use alpheus_daemon::captures::screencopy::{CaptureError, Frame, Screencopy, to_rgba};
use alpheus_daemon::memory::release_memory;
use common::Sway;
use smithay_client_toolkit::delegate_dispatch2;
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_registry};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use smithay_client_toolkit::reexports::protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

struct Client {
    shm: Option<Shm>,
    mode: Option<(i32, i32)>,
}

impl ShmHandler for Client {
    fn shm_state(&mut self) -> &mut Shm {
        self.shm.as_mut().unwrap()
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Client {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } = event
            && f.contains(wl_output::Mode::Current)
        {
            state.mode = Some((width, height));
        }
    }
}

delegate_dispatch2!(Client);
delegate_noop!(Client: ignore ZwlrScreencopyManagerV1);

/// One connection to THIS sway, able to capture its output as often as asked.
struct Session {
    queue: EventQueue<Client>,
    client: Client,
    screencopy: Screencopy,
    output: wl_output::WlOutput,
}

impl Session {
    fn new(sway: &Sway) -> Session {
        let stream = UnixStream::connect(sway.runtime.join(&sway.wayland)).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&connection).unwrap();
        let qh = queue.handle();
        let mut client = Client { shm: None, mode: None };
        client.shm = Some(Shm::bind(&globals, &qh).unwrap());
        let screencopy = Screencopy::bind(&globals, &qh).expect("sway offers screencopy");
        let output = globals.bind::<wl_output::WlOutput, _, _>(&qh, 1..=4, ()).unwrap();
        queue.roundtrip(&mut client).unwrap();
        Session { queue, client, screencopy, output }
    }

    fn capture(&mut self) -> (Frame, (i32, i32)) {
        let qh = self.queue.handle();
        let (tx, rx) = mpsc::channel::<Result<Frame, CaptureError>>();
        self.screencopy.request(&self.output, self.client.shm.as_ref().unwrap(), &qh, Box::new(move |r| tx.send(r).unwrap()));
        for _ in 0..200 {
            self.queue.blocking_dispatch(&mut self.client).unwrap();
            if let Ok(r) = rx.recv_timeout(Duration::ZERO) {
                return (r.expect("the capture succeeded"), self.client.mode.expect("the output announced its mode"));
            }
        }
        panic!("no answer from the compositor");
    }
}

/// This process's anonymous memory in KiB (`smaps_rollup`).
fn anonymous_kib() -> u64 {
    let t = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    let line = t.lines().find(|l| l.starts_with("Anonymous:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn capture_output_returns_a_frame_of_the_output_size() {
    let sway = Sway::start("screencopy", 1);
    // A known colour, so the pixel conversion is checked against the real compositor.
    sway.command("output * bg #336699 solid_color");
    std::thread::sleep(Duration::from_millis(300));
    let (frame, mode) = Session::new(&sway).capture();
    assert_eq!((frame.width as i32, frame.height as i32), mode);
    let whole = Rect { x: 0, y: 0, width: frame.width, height: frame.height };
    let rgba = to_rgba(&frame, whole);
    assert_eq!(rgba.len(), frame.width as usize * frame.height as usize * 4);
    let centre = ((frame.height / 2 * frame.width + frame.width / 2) * 4) as usize;
    assert_eq!(rgba[centre..centre + 4], [0x33, 0x66, 0x99, 0xFF]);
    let png = encode_png(&rgba, frame.width, frame.height).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let reader = png::Decoder::new(std::io::Cursor::new(png.as_slice())).read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (frame.width, frame.height));
}

/// A capture's pool, buffer and pixels are all gone once it is answered: cycle after cycle, the
/// anonymous memory stays where it is (within 64 KiB). Measured after two warm-up cycles: the test
/// runs on a harness thread, whose glibc arena keeps one freed frame (3.6 MB) for reuse, where the
/// daemon's main thread gives it back to the system (`release_memory`); the return to the figure
/// from before the first capture is the memory suite's to check, on the daemon itself.
#[test]
#[ignore = "headless sway (nix develop)"]
fn a_capture_cycle_gives_its_memory_back() {
    let sway = Sway::start("screencopy-memory", 1);
    let mut session = Session::new(&sway);
    let cycle = |session: &mut Session| {
        drop(session.capture());
        release_memory();
    };
    let before = anonymous_kib();
    cycle(&mut session);
    cycle(&mut session);
    let steady = anonymous_kib();
    for _ in 0..8 {
        cycle(&mut session);
    }
    let last = anonymous_kib();
    eprintln!("anonymous KiB: before {before}, after two cycles {steady}, after ten {last}");
    assert!(last.abs_diff(steady) <= 64, "the cycles keep growing: {steady} -> {last}");
}
