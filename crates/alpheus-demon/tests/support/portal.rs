//! A fake desktop portal on a `PrivateBus`, for the tests only. It owns
//! `org.freedesktop.portal.Desktop` and serves `org.freedesktop.portal.Settings.ReadOne` for the
//! appearance's color-scheme: another setting gets the portal's NotFound, any other method
//! UnknownMethod. It emits `SettingChanged` on command. It is one `Peer` served by its own thread, like
//! the tray's fakes (support/sni.rs), and speaks raw D-Bus: never the daemon's code.

use super::{Peer, PrivateBus};
use alpheus_demon::session_bus::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE};
use dbus::Message;
use dbus::arg::Variant;
use dbus::message::MessageType;
use dbus::strings::ErrorName;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How long the fake waits on its socket before looking at its commands (as support/sni.rs).
const POLL: Duration = Duration::from_millis(20);

enum Command {
    Set(u32),
    Unrelated,
    Stop,
}

pub struct FakePortal {
    commands: Sender<Command>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakePortal {
    /// Owns the portal's name; ReadOne answers `v(u value)`.
    pub fn start(bus: &PrivateBus, value: u32) -> FakePortal {
        Self::spawn(bus, value, false, Duration::ZERO)
    }

    /// Same, but ReadOne answers `v(v(u value))`, as some portal backends do.
    pub fn start_nested(bus: &PrivateBus, value: u32) -> FakePortal {
        Self::spawn(bus, value, true, Duration::ZERO)
    }

    /// Same as `start`, but ReadOne is answered only after `delay` (a portal slow to start).
    pub fn start_late(bus: &PrivateBus, value: u32, delay: Duration) -> FakePortal {
        Self::spawn(bus, value, false, delay)
    }

    fn spawn(bus: &PrivateBus, value: u32, nested: bool, delay: Duration) -> FakePortal {
        let peer = Peer::connect(bus);
        peer.own(PORTAL_NAME);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (commands, rx) = mpsc::channel();
        let c = calls.clone();
        let thread = thread::spawn(move || serve(peer, value, nested, delay, c, rx));
        FakePortal { commands, calls, thread: Some(thread) }
    }

    /// The desktop switches: the value changes and SettingChanged(appearance, color-scheme, v(u)) goes out.
    pub fn set(&self, value: u32) {
        let _ = self.commands.send(Command::Set(value));
    }

    /// Two SettingChanged the bar must ignore: another key of the namespace, the same key elsewhere.
    pub fn emit_unrelated(&self) {
        let _ = self.commands.send(Command::Unrelated);
    }

    /// Every method call received, "Interface.Member(arg, arg)", in arrival order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Drop for FakePortal {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn setting_changed(namespace: &str, key: &str, value: u32) -> Message {
    Message::new_signal(PORTAL_PATH, SETTINGS_INTERFACE, "SettingChanged").unwrap().append3(namespace, key, Variant(value))
}

fn serve(peer: Peer, mut value: u32, nested: bool, delay: Duration, calls: Arc<Mutex<Vec<String>>>, commands: Receiver<Command>) {
    loop {
        while let Ok(c) = commands.try_recv() {
            match c {
                Command::Set(v) => {
                    value = v;
                    let _ = peer.channel.send(setting_changed(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, v));
                }
                Command::Unrelated => {
                    let _ = peer.channel.send(setting_changed(APPEARANCE_NAMESPACE, "contrast", 1));
                    let _ = peer.channel.send(setting_changed("org.gnome.desktop.interface", COLOR_SCHEME_KEY, 2));
                }
                Command::Stop => return,
            }
            peer.channel.flush();
        }
        match peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) if m.msg_type() == MessageType::MethodCall => {
                let interface = m.interface().map(|i| i.to_string()).unwrap_or_default();
                let member = m.member().map(|x| x.to_string()).unwrap_or_default();
                let args = m.read2::<&str, &str>().map(|(a, b)| format!("{a}, {b}")).unwrap_or_default();
                calls.lock().unwrap().push(format!("{interface}.{member}({args})"));
                let asked = m.read2::<&str, &str>().ok();
                if member == "ReadOne" {
                    thread::sleep(delay); // start_late: the answer comes `delay` after the call
                }
                let reply = match (interface.as_str(), member.as_str(), asked) {
                    (SETTINGS_INTERFACE, "ReadOne", Some((APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY))) if nested => {
                        m.method_return().append1(Variant(Variant(value)))
                    }
                    (SETTINGS_INTERFACE, "ReadOne", Some((APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY))) => {
                        m.method_return().append1(Variant(value))
                    }
                    (SETTINGS_INTERFACE, "ReadOne", _) => {
                        m.error(&ErrorName::from("org.freedesktop.portal.Error.NotFound"), c"no such setting")
                    }
                    _ => m.error(&ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod"), c"no such method"),
                };
                let _ = peer.channel.send(reply);
                peer.channel.flush();
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}
