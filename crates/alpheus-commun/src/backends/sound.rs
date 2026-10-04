//! Son : wpctl (PipeWire / WirePlumber).

use super::{run_ok, unparsable, BResult};
use crate::exec::Executor;
use std::sync::Arc;

const TOOL: &str = "wpctl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

/// Une sortie (sink) ou une entrée (source).
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

/// Cible d'un réglage de volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRef {
    DefaultSink,
    Id(u32),
}

pub trait Sound: Send + Sync {
    /// Sorties et entrées audio.
    fn status(&self) -> BResult<AudioStatus>;
    /// Volume de la sortie par défaut.
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
        // -l 1.0 : jamais au-delà de 100 %, quoi qu'on lui demande.
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

/// `Volume: 0.62` ou `Volume: 0.62 [MUTED]`.
pub fn parse_get_volume(out: &str) -> BResult<Volume> {
    let rest = out
        .trim()
        .strip_prefix("Volume:")
        .ok_or_else(|| unparsable(TOOL, "préfixe « Volume: » absent"))?;
    let muted = rest.contains("[MUTED]");
    let num = rest.replace("[MUTED]", "");
    let num = num.trim();
    let percent = to_percent(num).ok_or_else(|| unparsable(TOOL, format!("volume « {num} »")))?;
    Ok(Volume { percent, muted })
}

#[derive(PartialEq)]
enum Section {
    Other,
    Sinks,
    Sources,
}

/// Ligne de nœud, ex. `*  116. CasqueBT   [vol: 0.62]` (cadres déjà retirés).
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

/// Sorties et entrées de la section « Audio » de `wpctl status`.
pub fn parse_status(out: &str) -> BResult<AudioStatus> {
    const FRAME: [char; 5] = [' ', '│', '├', '└', '─'];
    let mut in_audio = false;
    let mut seen_audio = false;
    let mut section = Section::Other;
    let mut st = AudioStatus::default();
    for line in out.lines() {
        // Titres de premier niveau (« Audio », « Video »…) : sans indentation.
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
        return Err(unparsable(TOOL, "section « Audio » absente"));
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
    fn analyse_les_sorties_reelles() {
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
    fn analyse_les_entrees_sans_defaut_et_ignore_la_video() {
        let st = parse_status(STATUS).unwrap();
        assert_eq!(st.sources.len(), 3);
        assert!(st.sources.iter().all(|n| !n.default));
        assert_eq!(st.sources[2].id, 121);
        assert_eq!(st.sources[2].volume.percent, 35);
        assert!(!st.sinks.iter().chain(&st.sources).any(|n| n.name.contains("Camera")));
    }

    #[test]
    fn noeud_muet() {
        let out = "Audio\n ├─ Sinks:\n │  *   50. Casque                [vol: 0.40 MUTED]\n";
        let st = parse_status(out).unwrap();
        assert_eq!(st.sinks[0].volume, Volume { percent: 40, muted: true });
        assert!(st.sinks[0].default);
    }

    #[test]
    fn sans_section_audio_illisible() {
        assert!(matches!(parse_status("rien d'utile\n"), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn get_volume() {
        assert_eq!(parse_get_volume("Volume: 0.62\n").unwrap(), Volume { percent: 62, muted: false });
        assert_eq!(parse_get_volume("Volume: 0.30 [MUTED]\n").unwrap(), Volume { percent: 30, muted: true });
        assert!(parse_get_volume("n'importe quoi").is_err());
        assert!(parse_get_volume("Volume: abc").is_err());
    }

    #[test]
    fn commandes_exactes() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wpctl get-volume @DEFAULT_AUDIO_SINK@", "Volume: 0.62\n");
        f.ok("wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 0.65", "");
        f.ok("wpctl set-volume -l 1.0 64 1.00", "");
        f.ok("wpctl set-default 116", "");
        let s = WpctlSound::new(f.clone());
        assert_eq!(s.volume().unwrap().percent, 62);
        s.set_volume(NodeRef::DefaultSink, 65).unwrap();
        s.set_volume(NodeRef::Id(64), 150).unwrap(); // borné à 100 %
        s.set_default(116).unwrap();
    }

    #[test]
    fn outil_absent() {
        let s = WpctlSound::new(Arc::new(FakeExecutor::new()));
        assert_eq!(s.status(), Err(BackendError::Missing { tool: "wpctl".into() }));
    }
}
