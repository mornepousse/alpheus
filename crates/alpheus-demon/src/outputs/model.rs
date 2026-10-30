//! Head model of the output protocol: identity, mode resolution in whole Hz,
//! the DisplayLink 120 Hz cap, conversion to the shared `Output` and the
//! no-op diff. Pure: the protocol side lives in `wlr.rs`.

use alpheus_commun::displays::{Active, Mode, Output, Scale, Step};

/// DisplayLink docks cannot drive more than this (millihertz).
pub const DISPLAYLINK_MAX_MHZ: i32 = 120_000;
/// Modes kept per head; the rest are ignored.
pub const MAX_MODES: usize = 256;
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
    ((mhz + 500) / 1000).max(0) as u32
}

fn to_mode(m: &HeadMode) -> Mode {
    Mode { width: m.width.max(0) as u32, height: m.height.max(0) as u32, refresh: hz(m.mhz) }
}

/// Mode of the head that `want` names: same size, same rounded Hz; the
/// current one first, then the preferred, then the highest. Above the
/// DisplayLink cap, the highest mode under it (of that size, else of any
/// size, the preferred first).
pub fn resolve_mode(head: &Head, want: Mode) -> Option<usize> {
    let same_size = |m: &HeadMode| m.width as u32 == want.width && m.height as u32 == want.height;
    if head.displaylink && i64::from(want.refresh) * 1000 > i64::from(DISPLAYLINK_MAX_MHZ) {
        let under = |(_, m): &(usize, &HeadMode)| m.mhz <= DISPLAYLINK_MAX_MHZ;
        let sized = head.modes.iter().enumerate().filter(under).filter(|(_, m)| same_size(m));
        if let Some((i, _)) = sized.max_by_key(|(_, m)| m.mhz) {
            return Some(i);
        }
        let any = || head.modes.iter().enumerate().filter(under);
        return any()
            .find(|(_, m)| m.preferred)
            .or_else(|| any().max_by_key(|(_, m)| (i64::from(m.width) * i64::from(m.height), m.mhz)))
            .map(|(i, _)| i);
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
    let cur = &head.modes[head.current?];
    if !head.capped(cur) {
        return None;
    }
    resolve_mode(head, Mode { width: cur.width as u32, height: cur.height as u32, refresh: hz(cur.mhz) })
}

/// The head as the shared model: modes in head order, one per `Mode`, the
/// capped ones left out; `active` from the current mode even when it is one
/// of them.
pub fn to_output(head: &Head) -> Output {
    let mut modes: Vec<Mode> = Vec::new();
    for m in head.modes.iter().filter(|m| !head.capped(m)) {
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

/// Desired state of one head: (mode index, x, y, scale, transform);
/// `None` = disabled.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub state: Option<(usize, i32, i32, f64, u8)>,
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
                Some((i, a.x, a.y, a.scale.as_f64(), a.transform))
            }
            Some(_) => None,
            None => h.current.filter(|_| h.enabled).map(|c| (c, h.x, h.y, h.scale, h.transform)),
        };
        out.push(Target { name: h.name.clone(), state });
    }
    if out.iter().all(|t| t.state.is_none()) {
        return Err("refused: no screen would stay on".into());
    }
    Ok(out)
}

/// Whether applying `targets` would change nothing.
pub fn is_noop(heads: &[Head], targets: &[Target]) -> bool {
    heads.len() == targets.len()
        && heads.iter().all(|h| {
            let Some(t) = targets.iter().find(|t| t.name == h.name) else { return false };
            match (&t.state, h.enabled, h.current) {
                (None, false, _) => true,
                (Some((i, x, y, s, tr)), true, Some(c)) => {
                    *i == c && *x == h.x && *y == h.y && *tr == h.transform && (s - h.scale).abs() <= SCALE_TOLERANCE
                }
                _ => false,
            }
        })
}

#[cfg(test)]
mod tests;
