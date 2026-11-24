//! Sound: wpctl (PipeWire / WirePlumber).
//!
//! Never:
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/compositors.md (what
//! `wm-set` and `shell-ctl` stand for).

use super::{BResult, run_ok, unparsable};
use crate::exec::Executor;
use std::sync::Arc;

const TOOL: &str = "wpctl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

/// An output (sink) or an input (source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioNode {
    pub id: u32,
    pub name: String,
    pub volume: Volume,
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioStatus {
    pub sinks: Vec<AudioNode>,
    pub sources: Vec<AudioNode>,
}

/// Target of a volume change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRef {
    DefaultSink,
    Id(u32),
}

pub trait Sound: Send + Sync {
    /// Audio outputs and inputs.
    fn status(&self) -> BResult<AudioStatus>;
    /// Volume of the default output.
    fn volume(&self) -> BResult<Volume>;
    fn set_volume(&self, node: NodeRef, percent: u8) -> BResult<()>;
    fn set_default(&self, id: u32) -> BResult<()>;
}

pub struct WpctlSound {
    exec: Arc<dyn Executor>,
}

impl WpctlSound {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        WpctlSound { exec }
    }
}

impl Sound for WpctlSound {
    fn status(&self) -> BResult<AudioStatus> {
        parse_status(&run_ok(&*self.exec, TOOL, &["status"])?)
    }

    fn volume(&self) -> BResult<Volume> {
        parse_get_volume(&run_ok(&*self.exec, TOOL, &["get-volume", "@DEFAULT_AUDIO_SINK@"])?)
    }

    fn set_volume(&self, node: NodeRef, percent: u8) -> BResult<()> {
        let target = match node {
            NodeRef::DefaultSink => "@DEFAULT_AUDIO_SINK@".to_string(),
            NodeRef::Id(id) => id.to_string(),
        };
        let value = format!("{:.2}", f32::from(percent.min(100)) / 100.0);
        // -l 1.0: never beyond 100%, whatever is asked of it.
        run_ok(&*self.exec, TOOL, &["set-volume", "-l", "1.0", &target, &value]).map(|_| ())
    }

    fn set_default(&self, id: u32) -> BResult<()> {
        run_ok(&*self.exec, TOOL, &["set-default", &id.to_string()]).map(|_| ())
    }
}

fn to_percent(s: &str) -> Option<u8> {
    let v: f32 = s.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * 100.0).round().min(255.0) as u8)
}

/// `Volume: 0.62` or `Volume: 0.62 [MUTED]`.
pub fn parse_get_volume(out: &str) -> BResult<Volume> {
    let rest = out.trim().strip_prefix("Volume:").ok_or_else(|| unparsable(TOOL, "missing `Volume:` prefix"))?;
    let muted = rest.contains("[MUTED]");
    let num = rest.replace("[MUTED]", "");
    let num = num.trim();
    let percent = to_percent(num).ok_or_else(|| unparsable(TOOL, format!("volume `{num}`")))?;
    Ok(Volume { percent, muted })
}

#[derive(PartialEq)]
enum Section {
    Other,
    Sinks,
    Sources,
}

/// A node line, e.g. `*  116. CasqueBT   [vol: 0.62]` (frame already stripped).
fn parse_node(body: &str) -> Option<AudioNode> {
    let (default, rest) = match body.strip_prefix('*') {
        Some(r) => (true, r.trim_start()),
        None => (false, body),
    };
    let (id, rest) = rest.split_once(". ")?;
    let id: u32 = id.trim().parse().ok()?;
    let idx = rest.rfind("[vol:")?;
    let name = rest[..idx].trim().to_string();
    let vol = rest[idx + 5..].trim_end_matches(']').trim();
    let muted = vol.ends_with("MUTED");
    let percent = to_percent(vol.trim_end_matches("MUTED").trim())?;
    Some(AudioNode { id, name, volume: Volume { percent, muted }, default })
}

/// Outputs and inputs from the "Audio" section of `wpctl status`.
pub fn parse_status(out: &str) -> BResult<AudioStatus> {
    const FRAME: [char; 5] = [' ', '│', '├', '└', '─'];
    let mut in_audio = false;
    let mut seen_audio = false;
    let mut section = Section::Other;
    let mut st = AudioStatus::default();
    for line in out.lines() {
        // Top-level titles ("Audio", "Video"…): no indentation.
        if !line.trim().is_empty() && !line.starts_with(FRAME) {
            in_audio = line.trim() == "Audio";
            seen_audio |= in_audio;
            section = Section::Other;
            continue;
        }
        if !in_audio {
            continue;
        }
        let body = line.trim_start_matches(FRAME).trim_end();
        match body {
            "Sinks:" => section = Section::Sinks,
            "Sources:" => section = Section::Sources,
            "Devices:" | "Filters:" | "Streams:" => section = Section::Other,
            _ => {
                if let Some(node) = parse_node(body) {
                    match section {
                        Section::Sinks => st.sinks.push(node),
                        Section::Sources => st.sources.push(node),
                        Section::Other => {}
                    }
                }
            }
        }
    }
    if !seen_audio {
        return Err(unparsable(TOOL, "missing `Audio` section"));
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendError;
    use crate::exec::fake::FakeExecutor;

    const STATUS: &str = include_str!("../../tests/fixtures/wpctl-status.txt");

    #[test]
    fn parses_the_real_sinks() {
        let st = parse_status(STATUS).unwrap();
        assert_eq!(st.sinks.len(), 6);
        let def: Vec<_> = st.sinks.iter().filter(|n| n.default).collect();
        assert_eq!(def.len(), 1);
        assert_eq!(def[0].id, 116);
        assert_eq!(def[0].name, "CasqueBT");
        assert_eq!(def[0].volume, Volume { percent: 62, muted: false });
        assert_eq!(st.sinks[0].name, "Dock D6000 Dell");
        assert_eq!(st.sinks[0].volume.percent, 75);
        assert_eq!(st.sinks[1].name, "600 Series Chipset Family HD Audio HDMI / DisplayPort 1 Output");
    }

    #[test]
    fn parses_sources_without_a_default_and_ignores_video() {
        let st = parse_status(STATUS).unwrap();
        assert_eq!(st.sources.len(), 3);
        assert!(st.sources.iter().all(|n| !n.default));
        assert_eq!(st.sources[2].id, 121);
        assert_eq!(st.sources[2].volume.percent, 35);
        assert!(!st.sinks.iter().chain(&st.sources).any(|n| n.name.contains("Camera")));
    }

    #[test]
    fn muted_node() {
        let out = "Audio\n ├─ Sinks:\n │  *   50. Casque                [vol: 0.40 MUTED]\n";
        let st = parse_status(out).unwrap();
        assert_eq!(st.sinks[0].volume, Volume { percent: 40, muted: true });
        assert!(st.sinks[0].default);
    }

    #[test]
    fn no_audio_section_is_unreadable() {
        assert!(matches!(parse_status("nothing useful\n"), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn get_volume() {
        assert_eq!(parse_get_volume("Volume: 0.62\n").unwrap(), Volume { percent: 62, muted: false });
        assert_eq!(parse_get_volume("Volume: 0.30 [MUTED]\n").unwrap(), Volume { percent: 30, muted: true });
        assert!(parse_get_volume("anything at all").is_err());
        assert!(parse_get_volume("Volume: abc").is_err());
    }

    #[test]
    fn exact_commands() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wpctl get-volume @DEFAULT_AUDIO_SINK@", "Volume: 0.62\n");
        f.ok("wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 0.65", "");
        f.ok("wpctl set-volume -l 1.0 64 1.00", "");
        f.ok("wpctl set-default 116", "");
        let s = WpctlSound::new(f.clone());
        assert_eq!(s.volume().unwrap().percent, 62);
        s.set_volume(NodeRef::DefaultSink, 65).unwrap();
        s.set_volume(NodeRef::Id(64), 150).unwrap(); // clamped to 100%
        s.set_default(116).unwrap();
    }

    #[test]
    fn missing_tool() {
        let s = WpctlSound::new(Arc::new(FakeExecutor::new()));
        assert_eq!(s.status(), Err(BackendError::Missing { tool: "wpctl".into() }));
    }
}
