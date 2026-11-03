//! Head model of the output protocol: identity, mode resolution in whole Hz,
//! the DisplayLink 120 Hz cap, conversion to the shared `Output` and the
//! no-op diff. Pure: the protocol side lives in `wlr.rs`.

use alpheus_commun::displays::{Active, Mode, Output, Scale, Step};

/// DisplayLink docks cannot drive more than this (millihertz).
pub const DISPLAYLINK_MAX_MHZ: i32 = 120_000;
/// Modes kept per head; the rest are ignored.
pub const MAX_MODES: usize = 256;
/// The refusal of a configuration that leaves every screen off.
pub const NO_SCREEN: &str = "refused: no screen would stay on";
/// Two scales closer than this are the same.
const SCALE_TOLERANCE: f64 = 0.005;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadMode {
    pub width: i32,
    pub height: i32,
    pub mhz: i32,
    pub preferred: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Head {
    /// Connector.
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub enabled: bool,
    pub modes: Vec<HeadMode>,
    /// Index into `modes`.
    pub current: Option<usize>,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u8,
    pub displaylink: bool,
}

impl Head {
    /// `make model serial` without the empty parts when the head reports a
    /// make or a model, else its description.
    pub fn identity(&self) -> String {
        if self.make.is_empty() && self.model.is_empty() {
            return self.description.clone();
        }
        [self.make.as_str(), self.model.as_str(), self.serial.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    fn capped(&self, m: &HeadMode) -> bool {
        self.displaylink && m.mhz > DISPLAYLINK_MAX_MHZ
    }
}

/// Millihertz to whole Hz, rounded.
pub fn hz(mhz: i32) -> u32 {
    ((i64::from(mhz) + 500) / 1000).clamp(0, i64::from(u32::MAX)) as u32
}

/// Whether a mode has a usable size; negative sizes (garbage from the
/// compositor) are ignored everywhere.
fn sane(m: &HeadMode) -> bool {
    m.width > 0 && m.height > 0
}

/// Highest mode under the DisplayLink cap: of `size` if any, else (any size)
/// the preferred one, else the largest area, then the highest rate.
fn lowered(head: &Head, size: (i32, i32)) -> Option<usize> {
    let under = |(_, m): &(usize, &HeadMode)| sane(m) && m.mhz <= DISPLAYLINK_MAX_MHZ;
    let sized = head.modes.iter().enumerate().filter(under).filter(|(_, m)| (m.width, m.height) == size);
    if let Some((i, _)) = sized.max_by_key(|(_, m)| m.mhz) {
        return Some(i);
    }
    let any = || head.modes.iter().enumerate().filter(under);
    any()
        .find(|(_, m)| m.preferred)
        .or_else(|| any().max_by_key(|(_, m)| (i64::from(m.width) * i64::from(m.height), m.mhz)))
        .map(|(i, _)| i)
}

fn to_mode(m: &HeadMode) -> Mode {
    Mode { width: m.width.max(0) as u32, height: m.height.max(0) as u32, refresh: hz(m.mhz) }
}

/// Mode of the head that `want` names: same size, same rounded Hz; the
/// current one first, then the preferred, then the highest. Above the
/// DisplayLink cap, the highest mode under it (of that size, else of any
/// size, the preferred first).
pub fn resolve_mode(head: &Head, want: Mode) -> Option<usize> {
    let same_size = |m: &HeadMode| sane(m) && m.width as u32 == want.width && m.height as u32 == want.height;
    if head.displaylink && i64::from(want.refresh) * 1000 > i64::from(DISPLAYLINK_MAX_MHZ) {
        return lowered(head, (want.width as i32, want.height as i32));
    }
    let mut hits = head
        .modes
        .iter()
        .enumerate()
        .filter(|(_, m)| same_size(m) && hz(m.mhz) == want.refresh && !head.capped(m))
        .map(|(i, _)| i)
        .peekable();
    hits.peek()?;
    let hits: Vec<usize> = hits.collect();
    head.current
        .filter(|c| hits.contains(c))
        .or_else(|| hits.iter().copied().find(|&i| head.modes[i].preferred))
        .or_else(|| hits.iter().copied().max_by_key(|&i| head.modes[i].mhz))
}

/// For a DisplayLink head whose current mode is over the cap: the mode to
/// lower it to.
pub fn over_cap(head: &Head) -> Option<usize> {
    let cur = head.modes.get(head.current?)?;
    if !head.capped(cur) {
        return None;
    }
    // Straight to the cap branch: a rate of 120.0 to 120.5 Hz rounds to 120.
    lowered(head, (cur.width, cur.height))
}

/// The head as the shared model: modes in head order, one per `Mode`, the
/// capped ones left out; `active` from the current mode even when it is one
/// of them.
pub fn to_output(head: &Head) -> Output {
    let mut modes: Vec<Mode> = Vec::new();
    for m in head.modes.iter().filter(|m| sane(m) && !head.capped(m)) {
        let m = to_mode(m);
        if !modes.contains(&m) {
            modes.push(m);
        }
    }
    let active = head.current.and_then(|c| head.modes.get(c)).filter(|_| head.enabled).map(|m| Active {
        mode: to_mode(m),
        x: head.x,
        y: head.y,
        scale: Scale::from_f64(head.scale).unwrap_or(Scale::ONE),
        transform: head.transform,
    });
    Output {
        name: head.name.clone(),
        description: head.identity(),
        compositor_description: head.description.clone(),
        modes,
        displaylink: head.displaylink,
        active,
    }
}

/// Mode index of an enabled head whose mode is unknown: it is left as it is.
/// The applier must not send a mode for it, and `is_noop` takes it as
/// unchanged. Its other fields are the head's own, sanitised.
pub const KEEP: usize = usize::MAX;

/// Desired state of one head: (mode index or `KEEP`, x, y, scale,
/// transform); `None` = disabled. A scale that is not finite and positive
/// reads as 1.0 and a transform above 7 as 0.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub state: Option<(usize, i32, i32, f64, u8)>,
}

fn sane_scale(s: f64) -> f64 {
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

fn sane_transform(t: u8) -> u8 {
    if t > 7 { 0 } else { t }
}

/// Every head's target: its step if any, else what it is now.
pub fn targets(heads: &[Head], steps: &[Step]) -> Result<Vec<Target>, String> {
    if let Some(s) = steps.iter().find(|s| !heads.iter().any(|h| h.name == s.output)) {
        return Err(format!("{}: no such screen", s.output));
    }
    let mut out = Vec::with_capacity(heads.len());
    for h in heads {
        let state = match steps.iter().rev().find(|s| s.output == h.name) {
            Some(Step { active: Some(a), .. }) => {
                let i = resolve_mode(h, a.mode).ok_or_else(|| format!("{}: no such mode", h.name))?;
                Some((i, a.x, a.y, sane_scale(a.scale.as_f64()), sane_transform(a.transform)))
            }
            Some(_) => None,
            None => h.enabled.then(|| {
                let mode = h.current.filter(|&c| c < h.modes.len()).unwrap_or(KEEP);
                (mode, h.x, h.y, sane_scale(h.scale), sane_transform(h.transform))
            }),
        };
        out.push(Target { name: h.name.clone(), state });
    }
    if out.iter().all(|t| t.state.is_none()) {
        return Err(NO_SCREEN.into());
    }
    Ok(out)
}

/// Whether applying `targets` would change nothing.
pub fn is_noop(heads: &[Head], targets: &[Target]) -> bool {
    heads.len() == targets.len()
        && heads.iter().all(|h| {
            let Some(t) = targets.iter().find(|t| t.name == h.name) else { return false };
            match (&t.state, h.enabled) {
                (None, false) => true,
                (Some((i, x, y, s, tr)), true) => {
                    (*i == KEEP || Some(*i) == h.current)
                        && *x == h.x
                        && *y == h.y
                        && *tr == sane_transform(h.transform)
                        && (s - sane_scale(h.scale)).abs() <= SCALE_TOLERANCE
                }
                _ => false,
            }
        })
}

#[cfg(test)]
mod tests;
