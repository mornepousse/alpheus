//! The quick panel's Bluetooth (quick Bluetooth spec §§ 1–2), pure: the row's glyph and value, the
//! detail with the paired devices then the ones nearby (at most ROWS_MAX), its selection kept by
//! object path, its keys and clicks, and the BlueZ calls they ask for (`BtOrder`), which the wiring
//! hands to the acting connection (`sources/dbus/bt_act.rs`). The view comes from the D-Bus thread
//! (`sources/dbus/bt.rs`) only while the panel is open. Device names, aliases, addresses and object
//! paths never reach a log from here.

use crate::actions::Button;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
use crate::list;
use crate::quick::{Line, Question, QuickAction, run, text_row};
use crate::quick_wifi::VALUE_CELLS;
use crate::sources::dbus::bt_act::{ActCall, AgentId, AgentReply};
use crate::system::{BtAdapter, BtEntry, BtKind, BtScope, BtState, BtView};
use crate::text;
use std::time::Instant;

/// The most devices the detail lists, paired and nearby together (quick Bluetooth spec § 2).
pub const ROWS_MAX: usize = 32;
/// Material Design (Nerd Font): bluetooth, bluetooth-connect, bluetooth-off.
pub const BT_ON: char = '\u{f00af}';
pub const BT_CONNECTED: char = '\u{f00b1}';
pub const BT_OFF: char = '\u{f00b2}';
/// A device's glyph by its kind: headphones, keyboard, mouse, speaker, cellphone, and the
/// bluetooth-connect one for anything else.
pub const HEADSET: char = '\u{f02cb}';
pub const KEYBOARD: char = '\u{f030c}';
pub const MOUSE: char = '\u{f037d}';
pub const SPEAKER: char = '\u{f04c3}';
pub const PHONE: char = '\u{f03f2}';
pub const GENERIC: char = '\u{f00b1}';
/// The detail's note with the adapter off, and the nearby devices' heading.
pub const OFF_NOTE: &str = "Bluetooth is off";
pub const NEARBY: &str = "Nearby";
/// A paired, unconnected device's tag.
const PAIRED_TAG: &str = "paired";

/// What the panel asks of BlueZ: a call on the acting connection, or the agent's answer to one of
/// BlueZ's requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtOrder {
    Call(ActCall),
    Reply(AgentId, AgentReply),
}

/// The panel's Bluetooth: dropped whole with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bt {
    /// What the D-Bus thread last read; None: nothing yet, or BlueZ gone.
    pub view: Option<BtView>,
    /// The detail shows in place of the rows.
    pub open: bool,
    /// The selected device by object path; None: the header.
    pub selected: Option<String>,
    /// Discovery was asked for during this open (and the adapter has stayed powered since).
    pub discovering: bool,
    /// `rfkill unblock bluetooth` ran: power the adapter on once BlueZ reports it unblocked.
    pub power_after_unblock: bool,
}

/// The adapter's path and its power the other way, as a call.
fn powered(a: &BtAdapter, on: bool) -> BtOrder {
    BtOrder::Call(ActCall::Powered { adapter: a.path.clone(), on })
}

fn kind_glyph(kind: BtKind) -> char {
    match kind {
        BtKind::Headset => HEADSET,
        BtKind::Keyboard => KEYBOARD,
        BtKind::Mouse => MOUSE,
        BtKind::Speaker => SPEAKER,
        BtKind::Phone => PHONE,
        BtKind::Other => GENERIC,
    }
}

/// Orders as an action: Redraw when there are none.
fn orders(v: Vec<BtOrder>) -> QuickAction {
    if v.is_empty() { QuickAction::Redraw } else { QuickAction::Bluetooth(v) }
}

impl Bt {
    /// The row shows: BlueZ has an adapter (`Absent`, or the bar's state not read: hidden).
    pub fn shows(bt: Option<&BtState>) -> bool {
        matches!(bt, Some(BtState::Off | BtState::On { .. }))
    }

    fn adapter(&self) -> Option<&BtAdapter> {
        self.view.as_ref()?.adapter.as_ref()
    }

    fn powered(&self) -> bool {
        self.adapter().is_some_and(|a| a.powered)
    }

    pub fn glyph(&self, bt: &BtState) -> char {
        match bt {
            BtState::On { devices } if !devices.is_empty() => BT_CONNECTED,
            BtState::On { .. } => BT_ON,
            _ => BT_OFF,
        }
    }

    /// The row's value (quick Bluetooth spec § 1): the first connected device's name (the bar's
    /// order), else `on`, `off`, or `blocked` when rfkill holds the adapter off.
    pub fn value(&self, bt: &BtState) -> String {
        match bt {
            BtState::On { devices } => match devices.first() {
                Some(d) => {
                    let name = text::clean(&d.name);
                    if name.trim().is_empty() { "?".into() } else { truncate(&name, VALUE_CELLS) }
                }
                None => "on".into(),
            },
            _ if self.adapter().is_some_and(|a| a.blocked) => "blocked".into(),
            _ => "off".into(),
        }
    }

    /// The detail's devices (quick Bluetooth spec § 2): none unless the adapter is powered; the
    /// paired ones, connected first, then by name (any case), then the ones nearby in the reader's
    /// order; at most ROWS_MAX in all.
    pub fn devices(&self) -> Vec<&BtEntry> {
        let Some(v) = self.view.as_ref().filter(|_| self.powered()) else { return Vec::new() };
        let mut paired: Vec<&BtEntry> = v.paired.iter().collect();
        paired.sort_by_cached_key(|e| (!e.connected, e.label.to_lowercase(), e.path.clone()));
        paired.into_iter().chain(&v.nearby).take(ROWS_MAX).collect()
    }

    /// How many of `devices()` are paired: they come first.
    fn paired_listed(&self) -> usize {
        self.view.as_ref().filter(|_| self.powered()).map_or(0, |v| v.paired.len().min(ROWS_MAX))
    }

    /// Enter, →, a click on the row: the detail, the first device selected (the header without
    /// one); discovery starts when the adapter is powered.
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.selected = self.devices().first().map(|d| d.path.clone());
        match self.adapter().filter(|a| a.powered).map(|a| a.path.clone()) {
            Some(adapter) => {
                self.discovering = true;
                QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::StartDiscovery { adapter })])
            }
            None => QuickAction::Redraw,
        }
    }

    /// Back to the rows: discovery stops if this open started it.
    pub fn close_detail(&mut self) -> Vec<BtOrder> {
        self.open = false;
        self.selected = None;
        let mut v = Vec::new();
        if std::mem::take(&mut self.discovering)
            && let Some(a) = self.adapter()
        {
            v.push(BtOrder::Call(ActCall::StopDiscovery { adapter: a.path.clone() }));
        }
        v
    }

    /// What the D-Bus thread is to read: the detail's lists while it is open, the adapter while the
    /// row shows, nothing otherwise.
    pub fn scope(&self, row_shown: bool) -> BtScope {
        if self.open {
            BtScope::Detail
        } else if row_shown {
            BtScope::Row
        } else {
            BtScope::Closed
        }
    }

    /// The acting connection is wanted: the detail is open.
    pub fn wants_connection(&self) -> bool {
        self.open
    }

    /// The D-Bus thread's view, while the panel is open. None (BlueZ lost) closes the detail, its
    /// orders dropped (the acting connection goes with BlueZ). An adapter just unblocked after
    /// `rfkill` is powered on; an open detail on a powered adapter discovers, once per open (power
    /// back after an outage starts it again); the first view of an open selects the first device.
    pub fn set_view(&mut self, view: Option<BtView>, _now: Instant) -> QuickAction {
        let first = self.view.is_none();
        self.view = view;
        if self.view.is_none() {
            if self.open {
                let _dropped = self.close_detail();
            }
            self.discovering = false;
            return QuickAction::Redraw;
        }
        let mut v = Vec::new();
        match self.adapter().cloned() {
            Some(a) => {
                if !a.blocked && std::mem::take(&mut self.power_after_unblock) {
                    v.push(powered(&a, true));
                }
                if !a.powered {
                    // BlueZ ended it: power back starts it again.
                    self.discovering = false;
                } else if self.open && !self.discovering {
                    self.discovering = true;
                    v.push(BtOrder::Call(ActCall::StartDiscovery { adapter: a.path.clone() }));
                }
            }
            None => self.discovering = false,
        }
        if self.open && first && self.selected.is_none() {
            self.selected = self.devices().first().map(|d| d.path.clone());
        }
        orders(v)
    }

    /// Space on the row (quick Bluetooth spec § 1): the adapter's power the other way; with rfkill
    /// holding it off, `rfkill unblock bluetooth` first. Nothing without an adapter read.
    pub fn power(&mut self, bt: &BtState) -> QuickAction {
        if *bt == BtState::Absent {
            return QuickAction::Nothing;
        }
        self.toggle()
    }

    /// The view's adapter the other way (the row's and the header's Space).
    fn toggle(&mut self) -> QuickAction {
        let Some(a) = self.adapter() else { return QuickAction::Nothing };
        if a.blocked {
            self.power_after_unblock = true;
            return run(&["rfkill", "unblock", "bluetooth"], "bluetooth unblock");
        }
        QuickAction::Bluetooth(vec![powered(a, !a.powered)])
    }

    /// Before each use: a selected device no longer listed gives the selection to the header (it
    /// never jumps onto another device).
    pub fn fit(&mut self) {
        if let Some(p) = &self.selected
            && !self.devices().iter().any(|d| &d.path == p)
        {
            self.selected = None;
        }
    }

    /// The detail's lines: its header, a separator, then (once a view is read) `Bluetooth is off`,
    /// or the paired devices, the `Nearby` heading and the nearby ones.
    pub fn lines(&self) -> Vec<Line> {
        let mut v = vec![Line::BtHeader, Line::Separator];
        if self.view.is_none() {
            return v;
        }
        if !self.powered() {
            v.push(Line::BtNote(OFF_NOTE));
            return v;
        }
        let all = self.devices().len();
        let paired = self.paired_listed();
        v.extend((0..paired).map(Line::BtDevice));
        v.push(Line::BtNearby);
        v.extend((paired..all).map(Line::BtDevice));
        v
    }

    pub fn is_selected(&self, line: Line) -> bool {
        match line {
            Line::BtHeader => self.selected.is_none(),
            Line::BtDevice(i) => self.devices().get(i).is_some_and(|d| self.selected.as_deref() == Some(d.path.as_str())),
            _ => false,
        }
    }

    /// Escape or ← back to the rows; ↑ ↓ Home End over the header and the devices, never wrapping;
    /// Space on the header powers the adapter.
    pub fn key(&mut self, k: Key, _confirm: &mut Option<(Question, Instant)>, _now: Instant) -> QuickAction {
        let devices: Vec<String> = self.devices().iter().map(|d| d.path.clone()).collect();
        let at = self.selected.as_ref().and_then(|p| devices.iter().position(|d| d == p)).map_or(0, |i| i + 1);
        let last = devices.len();
        let to = match k {
            Key::Escape | Key::Left => return orders(self.close_detail()),
            Key::Char(' ') if self.selected.is_none() => return self.toggle(),
            Key::Up | Key::CtrlP => at.saturating_sub(1),
            Key::Down | Key::CtrlN => (at + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            _ => return QuickAction::Nothing,
        };
        self.select(to.checked_sub(1).and_then(|i| devices.get(i)).cloned())
    }

    /// The selection on `to`: Redraw when it moved.
    fn select(&mut self, to: Option<String>) -> QuickAction {
        if to == self.selected {
            return QuickAction::Nothing;
        }
        self.selected = to;
        QuickAction::Redraw
    }

    /// A press on a detail line (the left button only): the header goes back, a device is selected.
    pub fn press(&mut self, line: Line, b: Button, _confirm: &mut Option<(Question, Instant)>, _now: Instant) -> QuickAction {
        if b != Button::Left {
            return QuickAction::Nothing;
        }
        match line {
            Line::BtHeader => orders(self.close_detail()),
            Line::BtDevice(i) => match self.devices().get(i).map(|d| d.path.clone()) {
                Some(p) => self.select(Some(p)),
                None => QuickAction::Nothing,
            },
            _ => QuickAction::Nothing,
        }
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side).
    pub fn draw(&self, g: &mut Grid, line: Line, _confirm: Option<&(Question, Instant)>, selected: bool) {
        let end = g.width();
        match line {
            Line::BtHeader => text_row(g, None, "‹ Bluetooth", if self.powered() { "on" } else { "off" }, Style::MUTED, selected),
            Line::BtNearby => {
                g.write(1, end.saturating_sub(1), NEARBY, Style::MUTED);
            }
            Line::BtNote(note) => {
                g.write(3, end.saturating_sub(1), note, Style::MUTED);
            }
            Line::BtDevice(i) => {
                if let Some(d) = self.devices().get(i) {
                    let tag = if d.paired && !d.connected { PAIRED_TAG } else { "" };
                    device_row(g, d, tag, Style::MUTED, selected);
                }
            }
            _ => {}
        }
    }
}

/// "● 󰋋 CasqueBT" / "  󰌌 ClavierBT      paired" (quick Bluetooth spec § 2): ● on a connected one in
/// the accent, the kind's glyph, the label cut to fit (`?` when empty), the tag right-aligned one
/// cell from the end. Selected: the band across the whole row.
fn device_row(g: &mut Grid, d: &BtEntry, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    let tag_width = text_width(tag);
    let room = if tag_width > 0 { tag_width + 2 } else { 1 };
    let label = if d.label.trim().is_empty() { "?" } else { d.label.as_str() };
    let label = truncate(label, end.saturating_sub(5).saturating_sub(room));
    let text = format!("{} {label}", kind_glyph(d.kind));
    let mark = if d.connected { '●' } else { ' ' };
    list::row(
        g,
        0,
        end,
        &list::Row { icon: Some(mark), icon_style: Style::ACCENT, text: &text, right: "", style: Style::NORMAL, selected },
    );
    if tag_width > 0 {
        g.write(end.saturating_sub(tag_width + 1), end, tag, if selected { Style::SELECTED } else { tag_style });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quick::{WIDTH, argv};
    use crate::system::BtDevice;
    use std::time::Duration;

    const ADAPTER: &str = "/org/bluez/hci0";

    fn path(n: u8) -> String {
        format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}")
    }

    fn entry(n: u8, label: &str, kind: BtKind, paired: bool, connected: bool) -> BtEntry {
        BtEntry { path: path(n), label: label.into(), kind, paired, trusted: paired, connected }
    }

    fn view(powered: bool, paired: Vec<BtEntry>, nearby: Vec<BtEntry>) -> BtView {
        BtView { adapter: Some(BtAdapter { path: ADAPTER.into(), powered, blocked: false }), paired, nearby }
    }

    fn blocked() -> BtView {
        BtView { adapter: Some(BtAdapter { path: ADAPTER.into(), powered: false, blocked: true }), ..BtView::default() }
    }

    fn paired() -> Vec<BtEntry> {
        vec![
            entry(1, "CasqueBT", BtKind::Headset, true, true),
            entry(2, "ClavierBT", BtKind::Keyboard, true, false),
            entry(5, "baffle-BT", BtKind::Speaker, true, false),
        ]
    }

    /// Paired, in path order: CasqueBT (01, connected), ClavierBT (02), baffle-BT (05); nearby in
    /// reader order: Speaker-X (04), Keyboard-Test (03).
    fn workstation() -> BtView {
        view(
            true,
            paired(),
            vec![entry(4, "Speaker-X", BtKind::Speaker, false, false), entry(3, "Keyboard-Test", BtKind::Keyboard, false, false)],
        )
    }

    fn opened() -> Bt {
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        b.open_detail();
        b
    }

    fn call(c: ActCall) -> QuickAction {
        QuickAction::Bluetooth(vec![BtOrder::Call(c)])
    }

    fn texts(b: &Bt, confirm: Option<&(Question, Instant)>) -> Vec<String> {
        b.lines()
            .into_iter()
            .map(|l| {
                let mut g = Grid::empty(WIDTH + 2);
                b.draw(&mut g, l, confirm, b.is_selected(l));
                g.text().trim_end().to_string()
            })
            .collect()
    }

    fn labels(b: &Bt) -> Vec<&str> {
        b.devices().iter().map(|d| d.label.as_str()).collect()
    }

    fn key(b: &mut Bt, k: Key) -> QuickAction {
        b.key(k, &mut None, Instant::now())
    }

    #[test]
    fn row_value_in_every_state() {
        assert!(!Bt::shows(None), "the bar's state not read yet");
        assert!(!Bt::shows(Some(&BtState::Absent)));
        assert!(Bt::shows(Some(&BtState::Off)));
        assert!(Bt::shows(Some(&BtState::On { devices: vec![] })));
        let b = Bt::default();
        let both = |s: &BtState| (b.value(s), b.glyph(s));
        assert_eq!(both(&BtState::Off), ("off".into(), BT_OFF));
        assert_eq!(both(&BtState::On { devices: vec![] }), ("on".into(), BT_ON));
        let two = BtState::On {
            devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(80) }, BtDevice { name: "ClavierBT".into(), battery: None }],
        };
        assert_eq!(both(&two), ("CasqueBT".into(), BT_CONNECTED), "the first, as the bar orders them");
        let long = BtState::On { devices: vec![BtDevice { name: "CasqueBT-".repeat(5), battery: None }] };
        let v = b.value(&long);
        assert_eq!(text_width(&v), 16);
        assert!(v.ends_with('…'), "{v}");
        let hostile = BtState::On { devices: vec![BtDevice { name: "\u{202e}\n".into(), battery: None }] };
        assert_eq!(b.value(&hostile), "?", "nothing left once cleaned");
        let b = Bt { view: Some(blocked()), ..Bt::default() };
        assert_eq!((b.value(&BtState::Off), b.glyph(&BtState::Off)), ("blocked".into(), BT_OFF), "rfkill holds it off");
    }

    #[test]
    fn space_powers_or_unblocks() {
        let mut on = Bt { view: Some(view(true, vec![], vec![])), ..Bt::default() };
        assert_eq!(on.power(&BtState::On { devices: vec![] }), call(ActCall::Powered { adapter: ADAPTER.into(), on: false }));
        let mut b = Bt { view: Some(blocked()), ..Bt::default() };
        assert_eq!(
            b.power(&BtState::Off),
            QuickAction::Run { argv: argv(&["rfkill", "unblock", "bluetooth"]), what: "bluetooth unblock".into() }
        );
        assert!(b.power_after_unblock);
        let t = Instant::now();
        assert_eq!(
            b.set_view(Some(view(false, vec![], vec![])), t),
            call(ActCall::Powered { adapter: ADAPTER.into(), on: true }),
            "unblocked: powered on"
        );
        assert!(!b.power_after_unblock, "once");
        assert_eq!(b.set_view(Some(view(false, vec![], vec![])), t), QuickAction::Redraw, "never twice");
        assert_eq!(Bt::default().power(&BtState::Off), QuickAction::Nothing, "no adapter read");
        assert_eq!(on.power(&BtState::Absent), QuickAction::Nothing);
    }

    #[test]
    fn detail_order_paired_then_nearby() {
        let b = opened();
        assert_eq!(
            b.lines(),
            [
                Line::BtHeader,
                Line::Separator,
                Line::BtDevice(0),
                Line::BtDevice(1),
                Line::BtDevice(2),
                Line::BtNearby,
                Line::BtDevice(3),
                Line::BtDevice(4)
            ]
        );
        assert_eq!(labels(&b), ["CasqueBT", "baffle-BT", "ClavierBT", "Speaker-X", "Keyboard-Test"], "by name in any case");
        assert_eq!(
            texts(&b, None),
            [
                format!(" ‹ Bluetooth{}on", " ".repeat(19)),
                String::new(),
                format!(" ● {HEADSET} CasqueBT"),
                format!("   {SPEAKER} baffle-BT{}paired", " ".repeat(13)),
                format!("   {KEYBOARD} ClavierBT{}paired", " ".repeat(13)),
                " Nearby".into(),
                format!("   {SPEAKER} Speaker-X"),
                format!("   {KEYBOARD} Keyboard-Test"),
            ]
        );
    }

    /// Review Focus 4: a hostile alias fits its row; an empty one reads `?`.
    #[test]
    fn an_empty_or_long_label_fits_its_row() {
        let one = |e: BtEntry| {
            let mut b = Bt { view: Some(view(true, vec![e], vec![])), ..Bt::default() };
            b.open_detail();
            texts(&b, None)[2].clone()
        };
        let long = "A".repeat(300);
        assert_eq!(one(entry(1, &long, BtKind::Keyboard, true, false)), format!("   {KEYBOARD} {}… paired", "A".repeat(20)));
        assert_eq!(one(entry(1, &long, BtKind::Headset, true, true)), format!(" ● {HEADSET} {}…", "A".repeat(27)));
        assert_eq!(one(entry(1, "", BtKind::Headset, true, true)), format!(" ● {HEADSET} ?"));
    }

    #[test]
    fn cap_32_rows() {
        let many = |n: std::ops::Range<u8>, paired: bool| -> Vec<BtEntry> {
            n.map(|n| entry(n, &format!("Device-{n}"), BtKind::Other, paired, false)).collect()
        };
        let mut b = Bt { view: Some(view(true, many(10..50, true), vec![])), ..Bt::default() };
        b.open_detail();
        assert_eq!(b.devices().len(), ROWS_MAX);
        assert_eq!(b.lines().len(), 2 + 32 + 1, "the header, the separator, 32 devices and the heading");
        let mut b = Bt { view: Some(view(true, many(10..30, true), many(30..50, false))), ..Bt::default() };
        b.open_detail();
        let d = b.devices();
        assert_eq!(d.len(), ROWS_MAX);
        assert_eq!(d.iter().filter(|e| e.paired).count(), 20, "the paired ones first");
        assert_eq!(d.iter().filter(|e| !e.paired).count(), 12, "then nearby, up to the cap");
        assert_eq!(b.lines()[22], Line::BtNearby);
    }

    #[test]
    fn glyph_per_kind() {
        for (kind, glyph) in [
            (BtKind::Headset, HEADSET),
            (BtKind::Keyboard, KEYBOARD),
            (BtKind::Mouse, MOUSE),
            (BtKind::Speaker, SPEAKER),
            (BtKind::Phone, PHONE),
            (BtKind::Other, GENERIC),
        ] {
            let mut b = Bt { view: Some(view(true, vec![entry(1, "X", kind, true, true)], vec![])), ..Bt::default() };
            b.open_detail();
            assert_eq!(texts(&b, None)[2], format!(" ● {glyph} X"), "{kind:?}");
        }
    }

    #[test]
    fn discovery_starts_once_per_open_and_stops_on_leave() {
        let t = Instant::now();
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        assert_eq!(b.open_detail(), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }));
        assert_eq!(b.set_view(Some(workstation()), t), QuickAction::Redraw, "once per open");
        assert_eq!(b.scope(true), BtScope::Detail);
        assert!(b.wants_connection());
        assert_eq!(b.close_detail(), vec![BtOrder::Call(ActCall::StopDiscovery { adapter: ADAPTER.into() })]);
        assert_eq!(b.close_detail(), vec![], "stopped once");
        assert_eq!((b.scope(true), b.scope(false)), (BtScope::Row, BtScope::Closed));
        assert!(!b.wants_connection());
        let mut b = Bt { view: Some(view(false, paired(), vec![])), ..Bt::default() };
        assert_eq!(b.open_detail(), QuickAction::Redraw, "off: nothing to discover");
        assert_eq!(b.set_view(Some(workstation()), t), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }), "powered on");
        assert_eq!(b.set_view(Some(workstation()), t), QuickAction::Redraw, "started once");
        assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), QuickAction::Redraw);
        assert!(!b.discovering, "BlueZ ended it with the power");
        assert_eq!(b.set_view(Some(workstation()), t), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }), "power back");
        let mut closed = Bt { view: Some(view(false, vec![], vec![])), ..Bt::default() };
        assert_eq!(closed.set_view(Some(workstation()), t), QuickAction::Redraw, "the detail closed: never discovering");
    }

    #[test]
    fn selection_kept_by_path_when_a_row_inserts_above() {
        let t = Instant::now();
        let mut b = opened();
        assert_eq!(b.selected, Some(path(1)), "the first device at open");
        assert_eq!(key(&mut b, Key::Down), QuickAction::Redraw);
        assert_eq!(key(&mut b, Key::Down), QuickAction::Redraw);
        assert_eq!(b.selected, Some(path(2)));
        let mut more = workstation();
        more.paired.push(entry(6, "aa-Test", BtKind::Keyboard, true, true));
        b.set_view(Some(more), t);
        b.fit();
        assert_eq!(b.selected, Some(path(2)), "by path");
        assert!(b.is_selected(Line::BtDevice(3)));
        let mut gone = workstation();
        gone.paired.retain(|e| e.path != path(2));
        b.set_view(Some(gone), t);
        b.fit();
        assert_eq!(b.selected, None, "gone: the header, never another device");
        assert!(b.is_selected(Line::BtHeader));
        assert_eq!(key(&mut b, Key::Up), QuickAction::Nothing, "never wrapping");
        assert_eq!(key(&mut b, Key::End), QuickAction::Redraw);
        assert_eq!(b.selected, Some(path(3)), "the last device");
        assert_eq!(key(&mut b, Key::Down), QuickAction::Nothing);
        assert_eq!(key(&mut b, Key::Home), QuickAction::Redraw);
        assert_eq!(b.selected, None);
        assert_eq!(b.press(Line::BtDevice(1), Button::Left, &mut None, t), QuickAction::Redraw, "a click selects");
        assert_eq!(b.selected, Some(path(5)));
        assert_eq!(b.press(Line::BtDevice(1), Button::Right, &mut None, t), QuickAction::Nothing);
        assert_eq!(key(&mut b, Key::Escape), call(ActCall::StopDiscovery { adapter: ADAPTER.into() }), "Escape leaves");
        assert!(!b.open);
        let mut b = opened();
        assert_eq!(
            b.press(Line::BtHeader, Button::Left, &mut None, t + Duration::from_millis(1)),
            call(ActCall::StopDiscovery { adapter: ADAPTER.into() })
        );
        assert!(!b.open, "the header goes back");
    }

    #[test]
    fn off_reads_bluetooth_is_off() {
        let mut b = Bt { view: Some(view(false, paired(), vec![])), ..Bt::default() };
        b.open_detail();
        assert_eq!(b.lines(), [Line::BtHeader, Line::Separator, Line::BtNote(OFF_NOTE)]);
        assert!(b.devices().is_empty());
        let t = texts(&b, None);
        assert_eq!(t[0], format!(" ‹ Bluetooth{}off", " ".repeat(18)));
        assert_eq!(t[2], "   Bluetooth is off");
        assert_eq!(key(&mut b, Key::Char(' ')), call(ActCall::Powered { adapter: ADAPTER.into(), on: true }), "Space on the header");
        let mut none = Bt::default();
        none.open_detail();
        assert_eq!(none.lines(), [Line::BtHeader, Line::Separator], "nothing read yet");
        assert_eq!(none.selected, None);
        none.set_view(Some(workstation()), Instant::now());
        assert_eq!(none.selected, Some(path(1)), "the first view of an open selects the first device");
    }
}
