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

/// The magic, a little-endian u32 length and type, the payload.
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

/// SUBSCRIBE to the two events on one connection, then GET_WORKSPACES and GET_TREE
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

/// After an event, the events already readable are drained without blocking, then
/// both requests are read once: a burst costs one re-read. 200 events of 64 bytes overflow
/// the reader's 8 KiB buffer, the 128th ending exactly at its end: the buffer is then empty,
/// and the rest is found by the zero-timeout poll alone.
#[test]
fn a_burst_of_events_costs_one_reread() {
    let f = FakeSway::start("burst", Script::default());
    let r = run(&f);
    r.states.recv_timeout(DELAY).unwrap();
    let event = reply(0x8000_0003, br#"{"change":"title","container":{"id":4},"pad":"xx"}"#);
    assert_eq!(event.len(), 64, "8 KiB holds 128 whole events");
    f.push(&event.repeat(200));
    assert_eq!(reads(&f, 4), [1, 4, 1, 4]);
    std::thread::sleep(QUIET);
    assert_eq!(reads(&f, 4).len(), 4, "200 events, one re-read");
    f.push(&window_event());
    assert_eq!(reads(&f, 6), [1, 4, 1, 4, 1, 4], "a later event: one more");
    assert!(r.states.recv_timeout(QUIET).is_err(), "the same state: not published again");
}

/// A burst is drained MAX_BURST events at a time, then read again: an endless stream of events
/// still refreshes the bar. 600 events: 256, 256 and 88, three re-reads.
#[test]
fn a_long_burst_is_read_again_every_max_burst_events() {
    let f = FakeSway::start("long-burst", Script::default());
    let r = run(&f);
    r.states.recv_timeout(DELAY).unwrap();
    assert_eq!(MAX_BURST, 256);
    f.push(&window_event().repeat(600));
    assert_eq!(reads(&f, 8), [1, 4, 1, 4, 1, 4, 1, 4]);
    std::thread::sleep(QUIET);
    assert_eq!(reads(&f, 8).len(), 8, "600 events, three re-reads");
}

#[test]
fn a_bad_magic_ends_the_session() {
    let f = FakeSway::start("magic", Script { subscribe: b"xx-ipc\x00\x00\x00\x00\x02\x00\x00\x00".to_vec(), ..Script::default() });
    let e = run(&f).ended();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("bad magic"), "{e}");
}

/// A reply over 4 MiB ends the session before its payload is read (the fake never
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

/// On the compositor thread's real 256 KiB stack (`spawn_source`'s): a tree nested
/// exactly MAX_NESTING levels is read and published; one level more ends the session as
/// unreadable, before serde_json recurses.
#[test]
fn a_deep_reply_is_bounded_on_the_compositor_stack() {
    let f = FakeSway::start("deep", Script { tree: nested(crate::compositor::MAX_NESTING), ..Script::default() });
    let r = Run::start(&f, Some(256 * 1024));
    let e = r.states.recv_timeout(DELAY).expect("a tree at MAX_NESTING is read");
    assert!(e.toplevels.is_empty());

    let f = FakeSway::start("deeper", Script { tree: nested(crate::compositor::MAX_NESTING + 1), ..Script::default() });
    let r = Run::start(&f, Some(256 * 1024));
    let e = r.ended();
    assert!(e.to_string().contains("unreadable sway tree"), "{e}");
    assert!(r.states.try_recv().is_err(), "nothing published");
}

/// The session feeds the window events to the model's focus history. C (10) then A
/// (7, the tree's focused window) focused: C previous first, B never seen, A last; the arrays
/// alone give B, C, A.
#[test]
fn focus_events_order_the_launchers_windows() {
    let f = FakeSway::start("focus", Script::default());
    let r = run(&f);
    let first = r.states.recv_timeout(DELAY).unwrap();
    let order = |e: &CompositorState| e.toplevels.iter().map(|t| t.selector.clone()).collect::<Vec<_>>();
    assert_eq!(order(&first), ["id:8", "id:10", "id:7"]);
    f.push(&reply(0x8000_0003, br#"{"change":"focus","container":{"id":10}}"#));
    f.push(&reply(0x8000_0003, br#"{"change":"focus","container":{"id":7}}"#));
    let next = r.states.recv_timeout(DELAY).unwrap();
    assert_eq!(order(&next), ["id:10", "id:8", "id:7"]);
}
