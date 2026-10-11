//! The quick panel's Wi-Fi (quick Wi-Fi spec), pure: the row's glyph and value, the detail with the
//! visible networks (one per SSID, its strongest access point, ordered, at most NETWORKS_MAX), its
//! keys and clicks, and the exact `nmcli` commands to run. The state comes from the D-Bus thread
//! (`sources/dbus/wifi.rs`) only while the panel is open; nothing here reads, spawns or keeps
//! anything once the panel closes.

use crate::actions::Button;
use crate::config::QuickConfig;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
use crate::list;
use crate::quick::{LOCK, Line, QuickAction, run, text_row};
use crate::system::{AccessPoint, Security, WifiDevice, WifiScope, WifiState};
use crate::text;
use std::cmp::Reverse;
use std::time::Instant;

/// The most networks the detail lists (quick Wi-Fi spec § 2).
pub const NETWORKS_MAX: usize = 32;
/// NMDeviceState: activated.
pub const ACTIVATED: u32 = 100;
/// The row's value: at most this many cells of the connected SSID.
pub const VALUE_CELLS: u16 = 16;
/// Material Design (Nerd Font): wifi-strength-1 to 4; wifi-strength-outline (on, not connected);
/// wifi-strength-off-outline (decision 5).
pub const STRENGTH: [char; 4] = ['\u{f091f}', '\u{f0922}', '\u{f0925}', '\u{f0928}'];
pub const WIFI_ON: char = '\u{f092f}';
pub const WIFI_OFF: char = '\u{f092e}';
/// The detail's notes (decision 11).
pub const OFF_NOTE: &str = "Wi-Fi is off";
pub const SEARCHING: &str = "Searching…";
pub const NO_NETWORKS: &str = "No networks";
const BARS: [char; 4] = ['▂', '▄', '▆', '█'];

/// 1 to 4 steps of signal: 0–24 %, 25–49 %, 50–74 %, 75–100 % (decision 5).
pub fn steps(strength: u8) -> usize {
    1 + usize::from(strength >= 25) + usize::from(strength >= 50) + usize::from(strength >= 75)
}

/// "▂▄▆█", "▂▄▆_", "▂▄__", "▂___".
pub fn bars(strength: u8) -> String {
    (0..BARS.len()).map(|i| if i < steps(strength) { BARS[i] } else { '_' }).collect()
}

/// An SSID as shown: lossy UTF-8, cleaned like any text from outside.
pub fn label(ssid: &[u8]) -> String {
    text::clean(&String::from_utf8_lossy(ssid))
}

/// An SSID that cannot be one nmcli argument as is (quick Wi-Fi spec § 5, decision 7): empty, a
/// NUL, a leading '-' (read as an option), or not UTF-8.
pub fn refused(ssid: &[u8]) -> bool {
    ssid.is_empty() || ssid.contains(&0) || ssid.first() == Some(&b'-') || std::str::from_utf8(ssid).is_err()
}

/// `[quick] wifi_more`, resolved at startup; unresolved (a test's default) or empty: no `More…`.
pub fn more(config: &QuickConfig) -> &[String] {
    config.wifi_more.as_deref().unwrap_or(&[])
}

/// `More…`: `wifi_more` as given, then the panel closes (decision 7); nothing when it is empty.
fn run_more(config: &QuickConfig) -> QuickAction {
    let argv = more(config).to_vec();
    if argv.is_empty() { QuickAction::Nothing } else { QuickAction::RunAndClose { argv, what: "wifi more".into() } }
}

/// What Enter does on a network (quick Wi-Fi spec § 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The one the device is on: disconnect, by its active connection's uuid (None: not known).
    Connected(Option<String>),
    /// A saved profile (by uuid): whatever its SSID.
    Saved(String),
    /// No key, an SSID nmcli can take.
    Open,
    /// WPA personal, not saved: the password field.
    Secured,
    /// 802.1X, WEP, other, or an SSID that cannot be an argument: `More…` instead.
    Unsupported,
}

/// One row of the detail: an SSID at its strongest access point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: Vec<u8>,
    pub label: String,
    pub strength: u8,
    pub security: Security,
    /// The first saved profile's uuid for this SSID.
    pub saved: Option<String>,
    pub connected: bool,
}

/// The detail's selection, by identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pick {
    #[default]
    None,
    Network(Vec<u8>),
    More,
}

/// The panel's Wi-Fi: dropped whole with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wifi {
    /// What the D-Bus thread last read; None: nothing yet, or NetworkManager gone.
    pub state: Option<WifiState>,
    /// The detail shows in place of the rows.
    pub open: bool,
    pub pick: Pick,
    /// Opened from the network module or `--quick wifi` (decision 12): the first state opens the detail.
    pub focus_pending: bool,
}

impl Wifi {
    fn device(&self) -> Option<&WifiDevice> {
        self.state.as_ref()?.device.as_ref()
    }

    /// The row shows: a Wi-Fi device.
    pub fn shows(&self) -> bool {
        self.device().is_some()
    }

    pub fn enabled(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.enabled)
    }

    /// The network the device is on: activated, with an access point.
    pub fn connected(&self) -> Option<&AccessPoint> {
        let d = self.device()?;
        if d.state == ACTIVATED { d.active.as_ref() } else { None }
    }

    pub fn glyph(&self) -> char {
        match self.connected() {
            _ if !self.enabled() => WIFI_OFF,
            Some(ap) => STRENGTH[steps(ap.strength) - 1],
            None => WIFI_ON,
        }
    }

    /// The row's value: the connected SSID, "on" or "off" (quick Wi-Fi spec § 1).
    pub fn value(&self) -> String {
        match self.connected() {
            _ if !self.enabled() => "off".into(),
            Some(ap) => truncate(&label(&ap.ssid), VALUE_CELLS),
            None => "on".into(),
        }
    }

    /// The detail's networks (quick Wi-Fi spec § 2): one per SSID (its strongest access point, the
    /// connected one included even when the list misses it), hidden ones left out; the connected
    /// one, the saved ones by signal, the others by signal (then by label); at most NETWORKS_MAX.
    /// None with the radio off.
    pub fn networks(&self) -> Vec<Network> {
        let Some(d) = self.device() else { return Vec::new() };
        if !self.enabled() {
            return Vec::new();
        }
        let connected = self.connected().map(|a| a.ssid.as_slice());
        let mut best: Vec<&AccessPoint> = Vec::new();
        for ap in d.access_points.iter().flatten().chain(self.connected()) {
            if ap.ssid.is_empty() {
                continue;
            }
            match best.iter_mut().find(|b| b.ssid == ap.ssid) {
                Some(b) if ap.strength > b.strength => *b = ap,
                Some(_) => {}
                None => best.push(ap),
            }
        }
        let saved = |ssid: &[u8]| d.saved.iter().flatten().find(|s| s.ssid == ssid).map(|s| s.uuid.clone());
        let mut v: Vec<Network> = best
            .into_iter()
            .map(|ap| Network {
                ssid: ap.ssid.clone(),
                label: label(&ap.ssid),
                strength: ap.strength,
                security: ap.security,
                saved: saved(&ap.ssid),
                connected: connected == Some(ap.ssid.as_slice()),
            })
            .collect();
        v.sort_by(|a, b| {
            (!a.connected, a.saved.is_none(), Reverse(a.strength), &a.label, &a.ssid).cmp(&(
                !b.connected,
                b.saved.is_none(),
                Reverse(b.strength),
                &b.label,
                &b.ssid,
            ))
        });
        v.truncate(NETWORKS_MAX);
        v
    }

    pub fn kind(&self, n: &Network) -> Kind {
        if n.connected {
            return Kind::Connected(self.device().and_then(|d| d.active_uuid.clone()));
        }
        if let Some(uuid) = &n.saved {
            return Kind::Saved(uuid.clone());
        }
        if refused(&n.ssid) {
            return Kind::Unsupported;
        }
        match n.security {
            Security::Open => Kind::Open,
            Security::Psk => Kind::Secured,
            Security::Enterprise | Security::Other => Kind::Unsupported,
        }
    }

    /// Space on the row: the radio the other way.
    pub fn radio(&self) -> QuickAction {
        if self.enabled() { run(&["nmcli", "radio", "wifi", "off"], "wifi off") } else { run(&["nmcli", "radio", "wifi", "on"], "wifi on") }
    }

    /// Enter, →, a click on the row: the detail, with one rescan when the radio is on.
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.pick = Pick::None;
        if self.enabled() { run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan") } else { QuickAction::Redraw }
    }

    pub fn close_detail(&mut self) {
        self.open = false;
        self.pick = Pick::None;
    }

    /// What the D-Bus thread is to read for the open panel.
    pub fn scope(&self) -> WifiScope {
        if self.open { WifiScope::Detail } else { WifiScope::Row }
    }

    /// The D-Bus thread's state (`Quick::set_wifi`): kept.
    pub fn set_state(&mut self, state: Option<WifiState>, _now: Instant) -> (QuickAction, Option<&'static str>) {
        self.state = state;
        (QuickAction::Redraw, None)
    }

    /// The picks in order: the networks, then `More…` when shown.
    fn picks(&self, nets: &[Network], config: &QuickConfig) -> Vec<Pick> {
        let mut v: Vec<Pick> = nets.iter().map(|n| Pick::Network(n.ssid.clone())).collect();
        if !more(config).is_empty() {
            v.push(Pick::More);
        }
        v
    }

    /// Before each use: no device, the detail closes; the selection back on a pick that exists (the
    /// first one when its own went).
    pub fn fit(&mut self, config: &QuickConfig) {
        if !self.shows() {
            self.close_detail();
            return;
        }
        if !self.open {
            return;
        }
        let picks = self.picks(&self.networks(), config);
        if !picks.contains(&self.pick) {
            self.pick = picks.into_iter().next().unwrap_or_default();
        }
    }

    /// The detail's lines: its header, a separator, the networks (or a note), and `More…` after a
    /// separator when configured.
    pub fn lines(&self, config: &QuickConfig) -> Vec<Line> {
        let mut v = vec![Line::WifiHeader, Line::Separator];
        let nets = self.networks();
        let listed = self.device().is_some_and(|d| d.access_points.is_some());
        if !self.enabled() {
            v.push(Line::WifiNote(OFF_NOTE));
        } else if nets.is_empty() {
            v.push(Line::WifiNote(if listed { NO_NETWORKS } else { SEARCHING }));
        } else {
            v.extend((0..nets.len()).map(Line::Network));
        }
        if !more(config).is_empty() {
            v.push(Line::Separator);
            v.push(Line::WifiMore);
        }
        v
    }

    pub fn is_selected(&self, line: Line, nets: &[Network]) -> bool {
        match (line, &self.pick) {
            (Line::Network(i), Pick::Network(s)) => nets.get(i).is_some_and(|n| n.ssid == *s),
            (Line::WifiMore, Pick::More) => true,
            _ => false,
        }
    }

    /// Keys in the detail: ↑ ↓ Home End over the networks and `More…` (never wrapping), Enter acts,
    /// Escape or ← go back to the rows.
    pub fn key(&mut self, config: &QuickConfig, k: Key, _now: Instant) -> QuickAction {
        let nets = self.networks();
        let picks = self.picks(&nets, config);
        let at = picks.iter().position(|p| *p == self.pick);
        let last = picks.len().saturating_sub(1);
        let to = match k {
            Key::Escape | Key::Left => {
                self.close_detail();
                return QuickAction::Redraw;
            }
            Key::Enter => return self.activate(config, &nets),
            Key::Up | Key::CtrlP => at.map_or(0, |a| a.saturating_sub(1)),
            Key::Down | Key::CtrlN => at.map_or(0, |a| (a + 1).min(last)),
            Key::Home => 0,
            Key::End => last,
            _ => return QuickAction::Nothing,
        };
        match picks.get(to) {
            Some(p) if *p != self.pick => {
                self.pick = p.clone();
                QuickAction::Redraw
            }
            _ => QuickAction::Nothing,
        }
    }

    fn activate(&mut self, config: &QuickConfig, nets: &[Network]) -> QuickAction {
        match self.pick.clone() {
            Pick::More => run_more(config),
            Pick::Network(ssid) => match nets.iter().find(|n| n.ssid == ssid) {
                Some(n) => self.enter(config, n),
                None => QuickAction::Nothing,
            },
            Pick::None => QuickAction::Nothing,
        }
    }

    /// Enter on `n` (quick Wi-Fi spec § 2): the exact nmcli command for its kind; `More…` chosen for
    /// one the panel does not handle.
    fn enter(&mut self, config: &QuickConfig, n: &Network) -> QuickAction {
        match self.kind(n) {
            Kind::Connected(Some(uuid)) => run(&["nmcli", "connection", "down", "uuid", &uuid], "wifi disconnect"),
            Kind::Connected(None) => QuickAction::Nothing,
            Kind::Saved(uuid) => run(&["nmcli", "connection", "up", "uuid", &uuid], "wifi connect (saved)"),
            Kind::Open => match std::str::from_utf8(&n.ssid) {
                Ok(ssid) => run(&["nmcli", "device", "wifi", "connect", ssid], "wifi connect (open)"),
                Err(_) => QuickAction::Nothing,
            },
            // The password field comes with the attempt (the next task replaces this arm).
            Kind::Secured => QuickAction::Nothing,
            Kind::Unsupported if !more(config).is_empty() => {
                self.pick = Pick::More;
                QuickAction::Redraw
            }
            Kind::Unsupported => QuickAction::Nothing,
        }
    }

    /// A press on a detail line (the left button only): a network is chosen and entered; `More…`
    /// runs; the header goes back.
    pub fn press(&mut self, config: &QuickConfig, line: Line, b: Button, _now: Instant) -> QuickAction {
        if b != Button::Left {
            return QuickAction::Nothing;
        }
        match line {
            Line::WifiHeader => {
                self.close_detail();
                QuickAction::Redraw
            }
            Line::Network(i) => {
                let nets = self.networks();
                let Some(n) = nets.get(i) else { return QuickAction::Nothing };
                let before = self.pick.clone();
                self.pick = Pick::Network(n.ssid.clone());
                let a = self.enter(config, n);
                if a == QuickAction::Nothing && self.pick != before { QuickAction::Redraw } else { a }
            }
            Line::WifiMore => {
                self.pick = Pick::More;
                run_more(config)
            }
            _ => QuickAction::Nothing,
        }
    }

    /// The tag after a network's marks: "saved" (dimmed), or nothing.
    fn tag(&self, n: &Network) -> (&'static str, Style) {
        if n.saved.is_some() { ("saved", Style::MUTED) } else { ("", Style::NORMAL) }
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side).
    pub fn draw(&self, g: &mut Grid, line: Line, nets: &[Network], selected: bool) {
        let end = g.width();
        match line {
            Line::WifiHeader => text_row(g, None, "‹ Wi-Fi", "", Style::NORMAL, false),
            Line::WifiNote(note) => {
                g.write(3, end.saturating_sub(1), note, Style::MUTED);
            }
            Line::WifiMore => text_row(g, Some(' '), "More…", "", Style::NORMAL, selected),
            Line::Network(i) => {
                if let Some(n) = nets.get(i) {
                    let (tag, style) = self.tag(n);
                    network_row(g, n, tag, style, selected);
                }
            }
            _ => {}
        }
    }
}

/// "● HomeNet-5G        ▂▄▆█ 🔒 saved" (quick Wi-Fi spec § 2): ● on the connected one in the accent,
/// the SSID cut to fit, then the signal, the lock for a secured one, and the tag, right-aligned one
/// cell from the end. Selected: the band across the whole row.
fn network_row(g: &mut Grid, n: &Network, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    let lock = if n.security.secured() { LOCK } else { ' ' };
    let signal = bars(n.strength);
    let right = if tag.is_empty() { format!("{signal} {lock}") } else { format!("{signal} {lock} {tag}") };
    let width = text_width(&right);
    let label = truncate(&n.label, end.saturating_sub(width + 2).saturating_sub(3));
    let mark = if n.connected { '●' } else { ' ' };
    list::row(
        g,
        0,
        end,
        &list::Row { icon: Some(mark), icon_style: Style::ACCENT, text: &label, right: "", style: Style::NORMAL, selected },
    );
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    let x = g.write(end.saturating_sub(width + 1), end, &signal, pick(Style::NORMAL));
    let x = g.write(x, end, " ", pick(Style::NORMAL));
    let x = g.write(x, end, &lock.to_string(), pick(Style::NORMAL));
    if !tag.is_empty() {
        let x = g.write(x, end, " ", pick(Style::NORMAL));
        g.write(x, end, tag, pick(tag_style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::{SavedWifi, WifiDevice};

    const HOME: &str = "11111111-2222-4333-8444-555555555555";
    const CAFE: &str = "33333333-4444-4555-8666-777777777777";

    fn ap(ssid: &str, strength: u8, security: Security) -> AccessPoint {
        AccessPoint { ssid: ssid.as_bytes().to_vec(), strength, security }
    }

    /// Connected to Maison-Test (80 %, WPA2, saved); Labo-Test (70 %, 802.1X), Bureau-Test (60 %,
    /// WPA2) and Cafe-Libre (40 %, open) around (fictional networks).
    fn state() -> WifiState {
        let home = ap("Maison-Test", 80, Security::Psk);
        WifiState {
            enabled: true,
            device: Some(WifiDevice {
                state: ACTIVATED,
                active: Some(home.clone()),
                active_uuid: Some(HOME.into()),
                access_points: Some(vec![
                    ap("Cafe-Libre", 40, Security::Open),
                    home,
                    ap("Bureau-Test", 60, Security::Psk),
                    ap("Labo-Test", 70, Security::Enterprise),
                ]),
                saved: Some(vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
            }),
        }
    }

    fn device(s: &mut WifiState) -> &mut WifiDevice {
        s.device.as_mut().expect("a device")
    }

    fn config() -> QuickConfig {
        QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() }
    }

    /// The detail open on `s`, fitted (the first network chosen).
    fn open(s: WifiState) -> Wifi {
        let mut w = Wifi { state: Some(s), ..Wifi::default() };
        w.open_detail();
        w.fit(&config());
        w
    }

    fn labels(w: &Wifi) -> Vec<String> {
        w.networks().iter().map(|n| n.label.clone()).collect()
    }

    fn run(parts: &[&str], what: &str) -> QuickAction {
        QuickAction::Run { argv: parts.iter().map(|s| s.to_string()).collect(), what: what.into() }
    }

    fn pick(w: &mut Wifi, label: &str) {
        w.pick = Pick::Network(label.as_bytes().to_vec());
    }

    #[test]
    fn four_steps_of_signal() {
        let cases = [
            (0, 1, "▂___"),
            (24, 1, "▂___"),
            (25, 2, "▂▄__"),
            (49, 2, "▂▄__"),
            (50, 3, "▂▄▆_"),
            (74, 3, "▂▄▆_"),
            (75, 4, "▂▄▆█"),
            (100, 4, "▂▄▆█"),
        ];
        for (strength, n, drawn) in cases {
            assert_eq!((steps(strength), bars(strength).as_str()), (n, drawn), "{strength} %");
        }
    }

    #[test]
    fn the_detail_orders_dedups_caps_and_hides() {
        assert_eq!(labels(&open(state())), ["Maison-Test", "Labo-Test", "Bureau-Test", "Cafe-Libre"], "connected, then by signal");
        let mut s = state();
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
        device(&mut s).access_points.as_mut().expect("listed").extend([ap("Bureau-Test", 90, Security::Open), ap("", 99, Security::Open)]);
        let w = open(s);
        assert_eq!(
            labels(&w),
            ["Maison-Test", "Cafe-Libre", "Bureau-Test", "Labo-Test"],
            "saved before stronger; one row per SSID; no hidden"
        );
        let bureau = &w.networks()[2];
        assert_eq!((bureau.strength, bureau.security), (90, Security::Open), "the strongest access point's");
        let mut s = state();
        device(&mut s).access_points = Some((0..40).map(|n| ap(&format!("Reseau-Test-{n:02}"), 50, Security::Psk)).collect());
        let w = open(s);
        assert_eq!(w.networks().len(), NETWORKS_MAX);
        assert_eq!(labels(&w)[0], "Maison-Test", "the connected one, even when the list misses it");
    }

    #[test]
    fn the_marks_and_the_rows() {
        let w = open(state());
        let nets = w.networks();
        let row = |line: Line, selected: bool| {
            let mut g = Grid::empty(crate::quick::WIDTH + 2);
            w.draw(&mut g, line, &nets, selected);
            g
        };
        assert_eq!(row(Line::Network(0), false).text().trim_end(), format!(" ● Maison-Test{}▂▄▆█ {LOCK} saved", " ".repeat(7)));
        assert_eq!(row(Line::Network(1), false).text().trim_end(), format!("   Labo-Test{}▂▄▆_ {LOCK}", " ".repeat(15)));
        assert_eq!(row(Line::Network(3), false).text().trim_end(), format!("   Cafe-Libre{}▂▄__", " ".repeat(14)), "open: no lock");
        let g = row(Line::Network(0), false);
        assert_eq!((&g.styles()[1..2], &g.styles()[28..33]), ("a", "mmmmm"), "● in the accent, saved dimmed");
        assert!(row(Line::Network(2), true).styles().chars().all(|c| c == 'S'), "selected: the band across the row");
        assert_eq!(row(Line::WifiHeader, false).text().trim_end(), " ‹ Wi-Fi");
        assert_eq!(row(Line::WifiMore, false).text().trim_end(), "   More…");
        assert_eq!(row(Line::WifiNote(OFF_NOTE), false).text().trim(), OFF_NOTE);
    }

    #[test]
    fn enter_on_each_kind_of_network() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Maison-Test");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "down", "uuid", HOME], "wifi disconnect"), "connected: down");
        let mut s = state();
        let d = device(&mut s);
        (d.state, d.active, d.active_uuid) = (30, None, None);
        let mut w = open(s);
        pick(&mut w, "Maison-Test");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", HOME], "wifi connect (saved)"));
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
        pick(&mut w, "Labo-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More, "802.1X: More… instead");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() });
        let mut w = open(state());
        pick(&mut w, "Labo-Test");
        assert_eq!(w.key(&QuickConfig { wifi_more: Some(vec![]), ..config() }, Key::Enter, t), QuickAction::Nothing, "no More…");
    }

    /// Review Focus 2: an SSID that cannot be one nmcli argument as is (a leading '-', a NUL, not
    /// UTF-8) is shown cleaned and never passed: More… unsaved, by uuid saved.
    #[test]
    fn refused_ssids_go_to_more_unless_saved() {
        let latin1 = vec![0x43, 0x61, 0x66, 0xE9];
        assert!(refused(b"-x") && refused(b"a\0b") && refused(&latin1) && refused(b""));
        assert!(!refused(b"Cafe-Libre") && !refused("Café".as_bytes()) && !refused(b"a-b"));
        let (c, t) = (config(), Instant::now());
        let mut s = state();
        let latin = AccessPoint { ssid: latin1.clone(), strength: 45, security: Security::Open };
        device(&mut s).access_points.as_mut().expect("listed").extend([ap("-x", 50, Security::Open), latin]);
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: latin1.clone() });
        let mut w = open(s);
        assert!(labels(&w).contains(&"Caf\u{fffd}".to_string()), "shown cleaned: {:?}", labels(&w));
        pick(&mut w, "-x");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More, "never `nmcli device wifi connect -x`");
        w.pick = Pick::Network(latin1);
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", CAFE], "wifi connect (saved)"), "saved: by uuid");
    }

    #[test]
    fn arrows_move_over_the_networks_and_more_never_wrapping() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the first network");
        assert_eq!(w.key(&c, Key::Up, t), QuickAction::Nothing);
        for _ in 0..3 {
            assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
        }
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More);
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Nothing, "the end: no wrap");
        assert_eq!(w.key(&c, Key::Home, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()));
        assert_eq!(w.key(&c, Key::End, t), QuickAction::Redraw);
        assert_eq!(w.key(&c, Key::Left, t), QuickAction::Redraw);
        assert!(!w.open, "← back to the rows");
    }

    #[test]
    fn the_detail_lines_and_its_notes() {
        let c = config();
        assert_eq!(
            open(state()).lines(&c),
            [
                Line::WifiHeader,
                Line::Separator,
                Line::Network(0),
                Line::Network(1),
                Line::Network(2),
                Line::Network(3),
                Line::Separator,
                Line::WifiMore
            ]
        );
        let mut s = state();
        let d = device(&mut s);
        (d.state, d.active, d.access_points) = (30, None, None);
        assert_eq!(open(s.clone()).lines(&c)[2], Line::WifiNote(SEARCHING), "before the list arrives");
        device(&mut s).access_points = Some(vec![]);
        assert_eq!(open(s).lines(&c)[2], Line::WifiNote(NO_NETWORKS));
        let mut off = state();
        off.enabled = false;
        let w = open(off);
        assert_eq!(w.lines(&c)[2..], [Line::WifiNote(OFF_NOTE), Line::Separator, Line::WifiMore], "the radio off: the list emptied");
        assert_eq!(w.pick, Pick::More);
        assert_eq!(open(state()).lines(&QuickConfig::default()).last(), Some(&Line::Network(3)), "no wifi_more: no More…, no separator");
    }

    #[test]
    fn networkmanager_going_away_closes_the_detail() {
        let t = Instant::now();
        let mut w = open(state());
        w.set_state(None, t);
        w.fit(&config());
        assert!(!w.open && !w.shows() && w.pick == Pick::None);
        let mut w = open(state());
        w.set_state(Some(WifiState { enabled: true, device: None }), t);
        w.fit(&config());
        assert!(!w.open, "the device gone");
    }

    #[test]
    fn a_press_acts_on_its_line() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        assert_eq!(
            w.press(&c, Line::Network(3), Button::Left, t),
            run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)")
        );
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        assert_eq!(w.press(&c, Line::Network(1), Button::Left, t), QuickAction::Redraw, "802.1X: More… chosen");
        assert_eq!(w.pick, Pick::More);
        assert_eq!(w.press(&c, Line::Network(0), Button::Right, t), QuickAction::Nothing, "the left button only");
        assert_eq!(
            w.press(&c, Line::WifiMore, Button::Left, t),
            QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() }
        );
        assert_eq!(w.press(&c, Line::WifiHeader, Button::Left, t), QuickAction::Redraw);
        assert!(!w.open, "the header goes back");
    }
}
