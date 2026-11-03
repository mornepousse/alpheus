//! The only file that speaks `zwlr_output_manager_v1` (displays spec § 1, § 2): heads and modes
//! followed as the compositor announces them, handed to the controller as one `Vec<Head>` at
//! each manager `done`, whose serial is the only one a configuration is ever created with. A
//! configuration is built and applied in one call (`send`), never across a dispatch, and checked
//! first: every head of that `done` exactly once, valid modes, so the compositor never has a
//! protocol error to raise against the bar's connection. Never anything about power.

use super::model::{Head, HeadMode, KEEP, MAX_MODES, Target};
use crate::log::write as log;
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_output::Transform;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
use smithay_client_toolkit::reexports::protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use std::sync::Mutex;

const LOG_MODES: &str = "displays: a head has more than 256 modes; the rest are ignored";

/// A mode's properties, kept on its proxy as they arrive.
#[derive(Debug)]
pub(crate) struct ModeData(Mutex<HeadMode>);

impl Default for ModeData {
    fn default() -> Self {
        ModeData(Mutex::new(HeadMode { width: 0, height: 0, mhz: 0, preferred: false }))
    }
}

impl ModeData {
    fn update(&self, f: impl FnOnce(&mut HeadMode)) {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

fn mode_of(p: &ZwlrOutputModeV1) -> HeadMode {
    p.data::<ModeData>().map(|d| d.0.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or(HeadMode {
        width: 0,
        height: 0,
        mhz: 0,
        preferred: false,
    })
}

/// A head as announced so far; its `modes` field is filled at `done`.
struct LiveHead {
    proxy: ZwlrOutputHeadV1,
    head: Head,
    modes: Vec<ZwlrOutputModeV1>,
    current: Option<ZwlrOutputModeV1>,
}

/// A head of the last `done` as the controller was given it: its connector, its proxy and the
/// proxies its mode indices name.
struct Shown {
    name: String,
    proxy: ZwlrOutputHeadV1,
    modes: Vec<ZwlrOutputModeV1>,
}

pub(crate) struct Wlr {
    manager: ZwlrOutputManagerV1,
    heads: Vec<LiveHead>,
    /// The serial of the last `done`.
    serial: Option<u32>,
    /// The heads of the last `done`, in the controller's order.
    shown: Vec<Shown>,
    modes_logged: bool,
}

/// A configuration the compositor would refuse with a protocol error (which ends the bar's
/// connection) or that names a head of no `done`: not sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unsendable;

impl Wlr {
    /// Binds the manager (versions 1 to 4); None when the compositor has none.
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Wlr> {
        let manager = globals.bind::<ZwlrOutputManagerV1, _, _>(qh, 1..=4, ()).ok()?;
        Some(Wlr { manager, heads: Vec::new(), serial: None, shown: Vec::new(), modes_logged: false })
    }

    /// Every head as it is at this `done`, its mode list capped at `MAX_MODES`.
    fn done(&mut self, serial: u32) -> Vec<Head> {
        self.serial = Some(serial);
        self.shown.clear();
        let mut heads = Vec::with_capacity(self.heads.len());
        for h in &self.heads {
            if h.modes.len() > MAX_MODES && !self.modes_logged {
                self.modes_logged = true;
                log(LOG_MODES);
            }
            let kept: Vec<ZwlrOutputModeV1> = h.modes.iter().take(MAX_MODES).cloned().collect();
            let mut head = h.head.clone();
            head.modes = kept.iter().map(mode_of).collect();
            head.current = h.current.as_ref().and_then(|c| kept.iter().position(|m| m == c));
            heads.push(head);
            self.shown.push(Shown { name: h.head.name.clone(), proxy: h.proxy.clone(), modes: kept });
        }
        heads
    }

    /// Builds the configuration of `targets` on the last `done`'s serial and applies it, in one
    /// go. Its answer comes back with `generation`.
    pub(crate) fn send(&self, generation: u64, targets: &[Target], qh: &QueueHandle<App>) -> Result<(), Unsendable> {
        let serial = self.serial.ok_or(Unsendable)?;
        let keys: Vec<(&str, usize)> = self.shown.iter().map(|s| (s.name.as_str(), s.modes.len())).collect();
        let order = plan(&keys, targets)?;
        let config = self.manager.create_configuration(serial, qh, generation);
        for (i, t) in order.into_iter().zip(targets) {
            let shown = &self.shown[i];
            match t.state {
                None => config.disable_head(&shown.proxy),
                Some((mode, x, y, scale, transform)) => {
                    let ch = config.enable_head(&shown.proxy, qh, ());
                    if mode != KEEP {
                        ch.set_mode(&shown.modes[mode]);
                    }
                    ch.set_position(x, y);
                    if let Ok(t) = Transform::try_from(u32::from(transform)) {
                        ch.set_transform(t);
                    }
                    ch.set_scale(scale);
                }
            }
        }
        config.apply();
        Ok(())
    }

    fn head(&mut self, p: &ZwlrOutputHeadV1) -> Option<&mut LiveHead> {
        self.heads.iter_mut().find(|h| h.proxy == *p)
    }
}

/// For each target, the index of its head among the last `done`'s (connector, mode count), or
/// `Unsendable`: every head exactly once, a mode index of that head (or `KEEP`), a positive
/// finite scale and a transform of `wl_output`.
pub(crate) fn plan(shown: &[(&str, usize)], targets: &[Target]) -> Result<Vec<usize>, Unsendable> {
    if targets.len() != shown.len() {
        return Err(Unsendable);
    }
    let mut out: Vec<usize> = Vec::with_capacity(targets.len());
    for t in targets {
        let i = shown.iter().position(|(name, _)| *name == t.name).ok_or(Unsendable)?;
        if out.contains(&i) {
            return Err(Unsendable);
        }
        if let Some((mode, _, _, scale, transform)) = t.state
            && ((mode != KEEP && mode >= shown[i].1) || !(scale.is_finite() && scale > 0.0) || transform > 7)
        {
            return Err(Unsendable);
        }
        out.push(i);
    }
    Ok(out)
}

fn release_head(p: &ZwlrOutputHeadV1) {
    if p.version() >= 3 {
        p.release();
    }
}

fn release_mode(p: &ZwlrOutputModeV1) {
    if p.version() >= 3 {
        p.release();
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for App {
    fn event(state: &mut App, _: &ZwlrOutputManagerV1, event: zwlr_output_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
        let Some(wlr) = state.displays.as_mut().map(|d| &mut d.wlr) else { return };
        match event {
            zwlr_output_manager_v1::Event::Head { head } => wlr.heads.push(LiveHead {
                proxy: head,
                head: Head {
                    name: String::new(),
                    description: String::new(),
                    make: String::new(),
                    model: String::new(),
                    serial: String::new(),
                    enabled: false,
                    modes: Vec::new(),
                    current: None,
                    x: 0,
                    y: 0,
                    scale: 1.0,
                    transform: 0,
                    displaylink: false,
                },
                modes: Vec::new(),
                current: None,
            }),
            zwlr_output_manager_v1::Event::Done { serial } => {
                let heads = wlr.done(serial);
                state.displays_done(heads);
            }
            // The compositor stops the manager: nothing more comes, nothing is sent.
            zwlr_output_manager_v1::Event::Finished => state.displays_gone(),
            _ => {}
        }
    }

    event_created_child!(App, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for App {
    fn event(state: &mut App, proxy: &ZwlrOutputHeadV1, event: zwlr_output_head_v1::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
        use zwlr_output_head_v1::Event as E;
        let Some(wlr) = state.displays.as_mut().map(|d| &mut d.wlr) else { return };
        if let E::Finished = event {
            if let Some(i) = wlr.heads.iter().position(|h| h.proxy == *proxy) {
                let gone = wlr.heads.remove(i);
                gone.modes.iter().for_each(release_mode);
            }
            release_head(proxy);
            return;
        }
        let Some(h) = wlr.head(proxy) else { return };
        match event {
            E::Name { name } => h.head.name = name,
            E::Description { description } => h.head.description = description,
            E::Make { make } => h.head.make = make,
            E::Model { model } => h.head.model = model,
            E::SerialNumber { serial_number } => h.head.serial = serial_number,
            E::Mode { mode } => h.modes.push(mode),
            E::Enabled { enabled } => h.head.enabled = enabled != 0,
            E::CurrentMode { mode } => h.current = Some(mode),
            E::Position { x, y } => (h.head.x, h.head.y) = (x, y),
            E::Transform { transform } => {
                h.head.transform = match transform {
                    WEnum::Value(t) => u32::from(t) as u8,
                    WEnum::Unknown(v) => u8::try_from(v).unwrap_or(u8::MAX),
                }
            }
            E::Scale { scale } => h.head.scale = scale,
            _ => {}
        }
    }

    event_created_child!(App, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ModeData::default()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for App {
    fn event(
        state: &mut App,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        data: &ModeData,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        use zwlr_output_mode_v1::Event as E;
        match event {
            E::Size { width, height } => data.update(|m| (m.width, m.height) = (width, height)),
            E::Refresh { refresh } => data.update(|m| m.mhz = refresh),
            E::Preferred => data.update(|m| m.preferred = true),
            E::Finished => {
                if let Some(wlr) = state.displays.as_mut().map(|d| &mut d.wlr) {
                    for h in &mut wlr.heads {
                        h.modes.retain(|m| m != proxy);
                        if h.current.as_ref() == Some(proxy) {
                            h.current = None;
                        }
                    }
                }
                release_mode(proxy);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, u64> for App {
    fn event(
        state: &mut App,
        proxy: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        generation: &u64,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        use crate::outputs::control::Event as C;
        use zwlr_output_configuration_v1::Event as E;
        let answer = match event {
            E::Succeeded => C::Succeeded(*generation),
            E::Failed => C::Failed(*generation),
            E::Cancelled => C::Cancelled(*generation),
            _ => return,
        };
        // One answer per configuration: it is done with.
        proxy.destroy();
        state.displays_event(answer);
    }
}

// No events.
impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for App {
    fn event(
        _: &mut App,
        _: &ZwlrOutputConfigurationHeadV1,
        _: zwlr_output_configuration_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

#[cfg(test)]
mod tests;
