use super::*;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
    move |k| pairs.iter().find(|(c, _)| *c == k).map(|(_, v)| OsString::from(v))
}

/// The usual session-script order, with niri last: SWAYSOCK, then
/// HYPRLAND_INSTANCE_SIGNATURE, then MANGO_INSTANCE_SIGNATURE, then NIRI_SOCKET; an empty
/// variable counts as unset; the first one set decides, even when its value is unusable.
#[test]
fn detection_follows_nixos_configs_order() {
    let sway = Compositor::Sway { socket: "/run/user/1000/sway-ipc.1000.42.sock".into() };
    let hyprland = Compositor::Hyprland { directory: "/run/user/1000/hypr/abc".into() };
    let mango = Compositor::Mango { socket: "/run/user/1000/mango-42.sock".into() };
    let niri = Compositor::Niri { socket: "/run/niri.sock".into() };
    let all: &'static [(&str, &str)] = &[
        ("SWAYSOCK", "/run/user/1000/sway-ipc.1000.42.sock"),
        ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
        ("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-42.sock"),
        ("NIRI_SOCKET", "/run/niri.sock"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
    ];
    assert_eq!(detect(env(all)), Some(sway), "a nested Sway inherits the others: Sway wins");
    assert_eq!(detect(env(&all[1..])), Some(hyprland), "then Hyprland");
    assert_eq!(detect(env(&all[2..])), Some(mango.clone()), "then Mango");
    assert_eq!(detect(env(&all[3..4])), Some(niri), "then niri");
    assert_eq!(
        detect(env(&[("SWAYSOCK", ""), ("HYPRLAND_INSTANCE_SIGNATURE", ""), ("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-42.sock")])),
        Some(mango),
        "an empty variable counts as unset"
    );
    assert_eq!(detect(env(&[])), None);
    assert_eq!(
        detect(env(&[("SWAYSOCK", ""), ("HYPRLAND_INSTANCE_SIGNATURE", ""), ("MANGO_INSTANCE_SIGNATURE", ""), ("NIRI_SOCKET", "")])),
        None
    );
    assert_eq!(
        detect(env(&[("MANGO_INSTANCE_SIGNATURE", "mango-42.sock"), ("NIRI_SOCKET", "/run/niri.sock")])),
        None,
        "a relative Mango path: no compositor, never the next variable's"
    );
    assert_eq!(
        detect(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-42.sock")])),
        None,
        "Hyprland without XDG_RUNTIME_DIR: no compositor, not Mango"
    );
}

#[test]
fn mango_socket_is_the_variable_itself_absolute_only() {
    assert_eq!(
        mango_socket(env(&[("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-42.sock")])),
        Some("/run/user/1000/mango-42.sock".into())
    );
    assert_eq!(mango_socket(env(&[("MANGO_INSTANCE_SIGNATURE", "")])), None);
    assert_eq!(mango_socket(env(&[("MANGO_INSTANCE_SIGNATURE", "run/mango.sock")])), None);
    assert_eq!(mango_socket(env(&[])), None);
}

/// With none of them, one line naming all four; otherwise one line naming the backend
/// and its variable, never the path.
#[test]
fn the_start_line_names_the_backend_or_the_four_variables() {
    assert_eq!(
        describe(None),
        "no compositor detected (SWAYSOCK, HYPRLAND_INSTANCE_SIGNATURE, MANGO_INSTANCE_SIGNATURE, NIRI_SOCKET unset or empty): \
             the bar shows \"–\""
    );
    let cases = [
        (Compositor::Sway { socket: "/x/sway.sock".into() }, "compositor: Sway ($SWAYSOCK)"),
        (Compositor::Hyprland { directory: "/x/hypr/abc".into() }, "compositor: Hyprland ($HYPRLAND_INSTANCE_SIGNATURE)"),
        (Compositor::Mango { socket: "/x/mango.sock".into() }, "compositor: Mango ($MANGO_INSTANCE_SIGNATURE)"),
        (Compositor::Niri { socket: "/x/niri.sock".into() }, "compositor: niri ($NIRI_SOCKET)"),
    ];
    for (c, line) in cases {
        assert_eq!(describe(Some(&c)), line);
        assert!(!describe(Some(&c)).contains("/x/"), "never the path");
    }
}

#[test]
fn supervise_is_generic_over_the_published_value() {
    let sends = RefCell::new(Vec::new());
    let mut round = 0;
    supervise(
        |publish: &mut dyn FnMut(u8) -> bool| {
            round += 1;
            publish(round);
            publish(round + 10);
            Err(io::Error::other("fell"))
        },
        |v: Option<u8>| {
            let mut s = sends.borrow_mut();
            s.push(v);
            s.len() < 6
        },
        |_| {},
        clock(Duration::ZERO),
        |_| {},
    );
    assert_eq!(*sends.borrow(), [Some(1), Some(11), None, Some(2), Some(12), None]);
}

#[test]
fn spawn_source_names_its_thread_and_prefixes_its_log() {
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_source(
        "sound",
        |publish: &mut dyn FnMut(String) -> bool| {
            publish(std::thread::current().name().unwrap_or("").to_string());
            Ok(())
        },
        move |v: Option<String>| {
            let _ = tx.send(v);
            false // stop after the first publication
        },
    )
    .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Some("sound".to_string()));
}

#[test]
fn prefixed_prepends_the_source_name() {
    assert_eq!(prefixed("compositor", "source down: refused"), "compositor source down: refused");
}

#[test]
fn delay_doubles_up_to_30s() {
    let mut r = Backoff::new();
    let d: Vec<u64> = (0..8).map(|_| r.next_delay().as_secs()).collect();
    assert_eq!(d, [1, 2, 4, 8, 16, 30, 30, 30]);
    r.on_success();
    assert_eq!(r.next_delay(), Duration::from_secs(1));
}

/// Simulated clock: advances by `step` on each read.
fn clock(step: Duration) -> impl FnMut() -> Instant {
    let mut t = Instant::now();
    move || {
        t += step;
        t
    }
}

#[test]
fn outage_dash_then_backoff_with_growing_delay() {
    let delays = RefCell::new(Vec::new());
    let sends = RefCell::new(Vec::new());
    let mut round = 0;
    supervise(
        |publish| {
            round += 1;
            match round {
                1..=6 | 8 => Err(io::Error::other("refused")),
                7 => {
                    publish(CompositorState::default()); // initial state
                    publish(CompositorState::default()); // a stream event
                    Err(io::Error::other("fell"))
                }
                _ => {
                    publish(CompositorState::default());
                    Ok(())
                }
            }
        },
        |e| {
            let mut v = sends.borrow_mut();
            v.push(e.is_some());
            v.len() < 11 // the 11th send: the bar stops
        },
        |d| {
            let mut v = delays.borrow_mut();
            v.push(d.as_secs());
            assert!(v.len() < 100, "supervise never stopped");
        },
        clock(Duration::ZERO),
        |_| {},
    );
    assert_eq!(*delays.borrow(), [1, 2, 4, 8, 16, 30, 1, 2], "back to 1 s after round 7 succeeds");
    let (f, t) = (false, true);
    assert_eq!(*sends.borrow(), [f, f, f, f, f, f, t, t, f, f, t], "\"–\" (None) on every outage");
}

/// A bug in `session` (or in a library it calls) that panics must
/// neither bring the whole daemon down, nor freeze the bar: `supervise`
/// catches it (`catch_unwind`), treats it like an ordinary outage ("–",
/// growing delay, one log line), and keeps retrying the source
/// afterwards — exactly like for an `Err`.
#[test]
fn panic_in_session_handled_like_an_outage_then_retried() {
    let delays = RefCell::new(Vec::new());
    let sends = RefCell::new(Vec::new());
    let logs = RefCell::new(Vec::new());
    let mut round = 0;
    supervise(
        |publish| {
            round += 1;
            match round {
                1 => panic!("simulation: bug in the session"),
                2 => {
                    publish(CompositorState::default()); // initial state
                    publish(CompositorState::default()); // a stream event: success
                    Ok(())
                }
                _ => Err(io::Error::other("refused")),
            }
        },
        |e| {
            let mut v = sends.borrow_mut();
            v.push(e.is_some());
            v.len() < 5
        },
        |d| {
            let mut v = delays.borrow_mut();
            v.push(d.as_secs());
            assert!(v.len() < 100, "supervise never stopped");
        },
        clock(Duration::ZERO),
        |m| logs.borrow_mut().push(m.to_string()),
    );
    // Round 1: panic → delay 1 s. Round 2: success (2 publications) →
    // backoff reset to 1 s. Round 3: ordinary outage; the 5th send
    // ("–" after this round) stops the bar before the next `sleep`.
    assert_eq!(*delays.borrow(), [1, 1]);
    let (f, t) = (false, true);
    assert_eq!(*sends.borrow(), [f, t, t, f, f], "\"–\" after the panic, like after an ordinary outage");
    // A single line: on the panic (first), not on round 3 (neither
    // first, nor a recovery behind it) — same rule as an Err.
    assert_eq!(logs.borrow().len(), 1, "{:?}", logs.borrow());
    // The panic's message can quote private text: the line names the panic and nothing else (the
    // daemon-wide hook logged its thread and place).
    assert_eq!(*logs.borrow(), ["source down: panic"]);
}

/// Defense in depth: even if a panic happens OUTSIDE the session watched
/// by `supervise` (already protected by `catch_unwind` — here, we
/// directly simulate a panic in the thread that carries it), the bar
/// must never stay frozen on its last state for lack of a final `None`
/// published.
#[test]
fn guard_publishes_dash_if_thread_panics() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let r = received.clone();
    let send = move |e: Option<CompositorState>| {
        r.lock().unwrap().push(e.is_some());
        true
    };
    let h = std::thread::spawn(move || {
        let _guard = SourceGuard { name: "compositor", send: &send, value: PhantomData };
        panic!("simulation: panic outside the watched session");
    });
    assert!(h.join().is_err(), "the thread must really panic, otherwise this test proves nothing");
    assert_eq!(*received.lock().unwrap(), [false], "\"–\" published by the guard despite the panic");
}

/// A source that serves the initial state and then immediately goes
/// down: the delay still grows, a single log line.
fn flapping(
    mut session: impl FnMut(&mut dyn FnMut(CompositorState) -> bool) -> io::Result<()>,
    step: Duration,
) -> (Vec<u64>, Vec<String>, usize) {
    let delays = RefCell::new(Vec::new());
    let log = RefCell::new(Vec::new());
    let published = RefCell::new(0);
    let cycles = RefCell::new(0);
    supervise(
        &mut session,
        |e| {
            if e.is_some() {
                *published.borrow_mut() += 1;
                return true;
            }
            *cycles.borrow_mut() += 1;
            *cycles.borrow() < 6
        },
        |d| {
            let mut v = delays.borrow_mut();
            v.push(d.as_secs());
            assert!(v.len() < 100, "supervise never stopped");
        },
        clock(step),
        |m| log.borrow_mut().push(m.to_string()),
    );
    (delays.into_inner(), log.into_inner(), published.into_inner())
}

#[test]
fn initial_state_alone_is_not_a_success() {
    let (delays, log, _) = flapping(
        |p| {
            p(CompositorState::default());
            Err(io::Error::other("fell"))
        },
        Duration::from_secs(1),
    );
    assert_eq!(delays, [1, 2, 4, 8, 16]);
    assert_eq!(log.len(), 1, "{log:?}");
}

#[test]
fn a_10s_session_counts_as_success() {
    let (delays, log, _) = flapping(
        |p| {
            p(CompositorState::default());
            Err(io::Error::other("fell"))
        },
        STABLE_DURATION,
    );
    assert_eq!(delays, [1, 1, 1, 1, 1]);
    assert_eq!(log.len(), 6, "one line per outage after each recovery");
}

#[test]
fn hyprland_flapper_backs_off() {
    let f = hypr::fake::FakeHyprland::start_with("flapper", hypr::fake::CLIENTS.as_bytes(), true);
    let d = f.directory.clone();
    let (delays, log, published) = flapping(|p| hypr::session(&d, p), Duration::ZERO);
    assert_eq!(published, 6, "the initial state served every round");
    assert_eq!(delays, [1, 2, 4, 8, 16]);
    assert_eq!(log.len(), 1, "{log:?}");
}

/// A sway that subscribes, answers, then closes, round after round: the delay
/// still grows and one line is logged.
#[test]
fn sway_flapper_backs_off() {
    let f = sway::fake::FakeSway::start("flapper", sway::fake::Script { flapper: true, ..Default::default() });
    let socket = f.socket.clone();
    let (delays, log, published) = flapping(|p| sway::session(&socket, false, p), Duration::ZERO);
    assert_eq!(published, 6, "the initial state served every round");
    assert_eq!(delays, [1, 2, 4, 8, 16]);
    assert_eq!(log.len(), 1, "{log:?}");
}

#[test]
fn niri_flapper_backs_off() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let directory = std::env::temp_dir().join(format!("alpheus-niri-flapper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("niri.sock");
    let l = UnixListener::bind(&path).unwrap();
    // Each connection: the full initial state, then close.
    let initial: String = include_str!("../../tests/fixtures/niri-evenements.jsonl").lines().take(6).map(|l| format!("{l}\n")).collect();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(c) = c else { return };
            let mut request = String::new();
            let _ = BufReader::new(&c).read_line(&mut request);
            let _ = (&c).write_all(initial.as_bytes());
        }
    });
    let (delays, log, published) = flapping(|p| niri::session(&path, p), Duration::ZERO);
    assert_eq!(published, 6, "one state published per round, once the full initial state has arrived");
    assert_eq!(delays, [1, 2, 4, 8, 16]);
    assert_eq!(log.len(), 1, "{log:?}");
    let _ = std::fs::remove_dir_all(&directory);
}

/// A Mango that serves both snapshots then closes, round after round (a
/// compositor restarting): the delay still grows, a single log line, no spin.
#[test]
fn mango_flapper_backs_off() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let directory = std::env::temp_dir().join(format!("alpheus-mango-flapper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("mango.sock");
    let l = UnixListener::bind(&path).unwrap();
    // Both watches are answered before either is dropped (after a short pause so the session
    // reads both replies first): a round that ends on the first close would publish nothing.
    std::thread::spawn(move || {
        loop {
            let Ok((first, _)) = l.accept() else { return };
            let Ok((second, _)) = l.accept() else { return };
            for c in [&first, &second] {
                let mut request = String::new();
                let _ = BufReader::new(c).read_line(&mut request);
                let reply = if request.contains("monitors") { mango::fake::MONITORS } else { mango::fake::CLIENTS };
                let _ = (&*c).write_all(format!("{}\n", reply.trim_end()).as_bytes());
            }
            std::thread::sleep(Duration::from_millis(50));
            // Both dropped here: Mango gone.
        }
    });
    let (delays, log, published) = flapping(|p| mango::session(&path, false, p), Duration::ZERO);
    assert!(published >= 1, "the state was served at least once");
    assert_eq!(delays, [1, 2, 4, 8, 16]);
    assert_eq!(log.len(), 1, "{log:?}");
    let _ = std::fs::remove_dir_all(&directory);
}

/// The Mango path is read again at each session; gone, the detected one.
#[test]
fn the_mango_path_is_read_again_at_each_session() {
    let detected = Path::new("/run/user/1000/mango-1.sock");
    assert_eq!(
        mango_path(env(&[("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-2.sock")]), detected),
        Path::new("/run/user/1000/mango-2.sock")
    );
    assert_eq!(mango_path(env(&[]), detected), detected);
    assert_eq!(mango_path(env(&[("MANGO_INSTANCE_SIGNATURE", "relative.sock")]), detected), detected);
}

#[test]
fn hyprland_signature_escaping_directory_refused() {
    for sig in ["../../etc", "a/b", ".."] {
        let v: &'static str = Box::leak(sig.to_string().into_boxed_str());
        let pairs: &'static [(&str, &str)] =
            Box::leak(Box::new([("HYPRLAND_INSTANCE_SIGNATURE", v), ("XDG_RUNTIME_DIR", "/run/user/1000")]));
        assert_eq!(detect(env(pairs)), None, "{sig}");
    }
}
