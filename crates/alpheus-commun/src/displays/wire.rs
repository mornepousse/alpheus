//! The text a displays snapshot and a desired state travel in, between the
//! daemon and the TUI, as the body of the socket's `outputs` verbs
//! (`launcher_ipc`). Line-based, fields separated by a tab, std only. Text a
//! screen reports (its names) is cleaned before it is written, and what is
//! read is checked: an unknown word, a wrong field count, a count past the
//! caps or a control character is refused, never guessed at.

use super::{Active, Mode, Output, Scale, Step};

/// Longest body, in bytes, on the wire either way.
pub const MAX_OUTPUTS_BODY: usize = 64 * 1024;
pub const MAX_OUTPUTS: usize = 16;
pub const MAX_MODES: usize = 256;
/// Longest text field, in characters, once cleaned.
const MAX_TEXT: usize = 128;

/// What the daemon says of its displays module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The compositor's output manager is bound and the heads are read.
    Ready,
    /// `[displays]` is off in `bar.toml`.
    Off,
    /// The compositor offers no output management.
    NoManager,
}

impl Status {
    const ALL: [Status; 3] = [Status::Ready, Status::Off, Status::NoManager];

    fn word(self) -> &'static str {
        match self {
            Status::Ready => "ready",
            Status::Off => "off",
            Status::NoManager => "no-manager",
        }
    }
}

/// How a trial ended, for the TUI's Keep box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialEnd {
    Kept,
    Reverted,
    ScreenLost,
    ChangedElsewhere,
    Refused,
    NoAnswer,
}

impl TrialEnd {
    const ALL: [TrialEnd; 6] =
        [TrialEnd::Kept, TrialEnd::Reverted, TrialEnd::ScreenLost, TrialEnd::ChangedElsewhere, TrialEnd::Refused, TrialEnd::NoAnswer];

    fn word(self) -> &'static str {
        match self {
            TrialEnd::Kept => "kept",
            TrialEnd::Reverted => "reverted",
            TrialEnd::ScreenLost => "screen-lost",
            TrialEnd::ChangedElsewhere => "changed-elsewhere",
            TrialEnd::Refused => "refused",
            TrialEnd::NoAnswer => "no-answer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialState {
    None,
    /// A trial is waiting for Keep or Revert.
    Open {
        seconds_left: u32,
    },
    Ended(TrialEnd),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub status: Status,
    pub outputs: Vec<Output>,
    pub trial: TrialState,
}

/// Control characters (tabs and newlines included) become spaces; at most `MAX_TEXT` characters.
fn clean(s: &str) -> String {
    s.chars().take(MAX_TEXT).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn active_field(a: &Option<Active>) -> String {
    match a {
        None => "off".to_string(),
        Some(a) => format!("{} {} {} {} {}", a.mode, a.x, a.y, a.scale.0, a.transform),
    }
}

pub fn encode_snapshot(s: &Snapshot) -> String {
    let mut out = format!("status {}\n", s.status.word());
    out.push_str(&match s.trial {
        TrialState::None => "trial none\n".to_string(),
        TrialState::Open { seconds_left } => format!("trial open {seconds_left}\n"),
        TrialState::Ended(e) => format!("trial ended {}\n", e.word()),
    });
    for o in s.outputs.iter().take(MAX_OUTPUTS) {
        let modes: Vec<String> = o.modes.iter().take(MAX_MODES).map(Mode::to_string).collect();
        let line = format!(
            "output\t{}\t{}\t{}\t{}\t{}\t{}\n",
            clean(&o.name),
            clean(&o.description),
            clean(&o.compositor_description),
            u8::from(o.displaylink),
            active_field(&o.active),
            modes.join(","),
        );
        if out.len() + line.len() > MAX_OUTPUTS_BODY {
            break;
        }
        out.push_str(&line);
    }
    out
}

fn lines_of(body: &str) -> Result<Vec<&str>, String> {
    if body.len() > MAX_OUTPUTS_BODY {
        return Err("the displays body is too large".into());
    }
    if body.is_empty() {
        return Ok(Vec::new());
    }
    Ok(body.strip_suffix('\n').unwrap_or(body).split('\n').collect())
}

fn text_field(s: &str) -> Result<String, String> {
    if s.chars().any(char::is_control) {
        return Err("a control character in a text field".into());
    }
    Ok(s.to_string())
}

fn number<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("{what} is not a number"))
}

fn parse_active(s: &str) -> Result<Option<Active>, String> {
    if s == "off" {
        return Ok(None);
    }
    let f: Vec<&str> = s.split(' ').collect();
    let [mode, x, y, scale, transform] = f.as_slice() else {
        return Err("an active field needs five words".into());
    };
    let scale: u32 = number(scale, "scale")?;
    Ok(Some(Active {
        mode: Mode::parse(mode).ok_or("unreadable mode")?,
        x: number(x, "x")?,
        y: number(y, "y")?,
        scale: Scale(scale),
        transform: number(transform, "transform")?,
    }))
}

fn parse_output(line: &str) -> Result<Output, String> {
    let f: Vec<&str> = line.split('\t').collect();
    let ["output", name, identity, compositor, displaylink, active, modes] = f.as_slice() else {
        return Err("an output line needs seven fields".into());
    };
    let displaylink = match *displaylink {
        "0" => false,
        "1" => true,
        _ => return Err("displaylink is 0 or 1".into()),
    };
    let modes = if modes.is_empty() {
        Vec::new()
    } else {
        let all: Vec<&str> = modes.split(',').collect();
        if all.len() > MAX_MODES {
            return Err("too many modes".into());
        }
        all.into_iter().map(|m| Mode::parse(m).ok_or_else(|| "unreadable mode".to_string())).collect::<Result<_, _>>()?
    };
    Ok(Output {
        name: text_field(name)?,
        description: text_field(identity)?,
        compositor_description: text_field(compositor)?,
        modes,
        displaylink,
        active: parse_active(active)?,
    })
}

pub fn decode_snapshot(body: &str) -> Result<Snapshot, String> {
    let lines = lines_of(body)?;
    let [status, trial, outputs @ ..] = lines.as_slice() else {
        return Err("a snapshot starts with its status and its trial".into());
    };
    let status = status.strip_prefix("status ").and_then(|w| Status::ALL.into_iter().find(|s| s.word() == w)).ok_or("unknown status")?;
    let trial = match trial.strip_prefix("trial ").map(|t| t.split(' ').collect::<Vec<_>>()).as_deref() {
        Some(["none"]) => TrialState::None,
        Some(["open", s]) => TrialState::Open { seconds_left: number(s, "seconds left")? },
        Some(["ended", w]) => TrialState::Ended(TrialEnd::ALL.into_iter().find(|e| e.word() == *w).ok_or("unknown trial word")?),
        _ => return Err("unknown trial line".into()),
    };
    if outputs.len() > MAX_OUTPUTS {
        return Err("too many outputs".into());
    }
    Ok(Snapshot { status, trial, outputs: outputs.iter().map(|l| parse_output(l)).collect::<Result<_, _>>()? })
}

/// The desired state: one `step` line per output, `step\t<name>\t<off | WxH@R x y scale transform>`.
pub fn encode_desired(steps: &[Step]) -> String {
    let mut out = String::new();
    for s in steps.iter().take(MAX_OUTPUTS) {
        out.push_str(&format!("step\t{}\t{}\n", clean(&s.output), active_field(&s.active)));
    }
    out
}

pub fn decode_desired(body: &str) -> Result<Vec<Step>, String> {
    let lines = lines_of(body)?;
    if lines.len() > MAX_OUTPUTS {
        return Err("too many steps".into());
    }
    lines
        .iter()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let ["step", name, active] = f.as_slice() else {
                return Err("a step line needs three fields".to_string());
            };
            Ok(Step { output: text_field(name)?, active: parse_active(active)? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: u32, h: u32, r: u32) -> Mode {
        Mode { width: w, height: h, refresh: r }
    }

    fn active(m: Mode, x: i32, y: i32, scale: u32, transform: u8) -> Active {
        Active { mode: m, x, y, scale: Scale(scale), transform }
    }

    fn output(name: &str, identity: &str, active: Option<Active>, displaylink: bool) -> Output {
        Output {
            name: name.into(),
            description: identity.into(),
            compositor_description: format!("{identity} (compositor)"),
            modes: vec![mode(1920, 1080, 60), mode(1280, 720, 60)],
            displaylink,
            active,
        }
    }

    fn sample() -> Snapshot {
        Snapshot {
            status: Status::Ready,
            outputs: vec![
                output("eDP-1", "Laptop panel", Some(active(mode(2560, 1600, 165), 0, 0, 1250, 0)), false),
                output("DVI-I-1", "Lenovo LEN T24i", Some(active(mode(1920, 1080, 60), -1920, 120, 1000, 3)), true),
                output("HDMI-A-1", "Dell U2415", None, false),
            ],
            trial: TrialState::Open { seconds_left: 12 },
        }
    }

    #[test]
    fn snapshot_round_trips() {
        let s = sample();
        assert_eq!(decode_snapshot(&encode_snapshot(&s)), Ok(s));
        for status in Status::ALL {
            for trial in
                [TrialState::None, TrialState::Open { seconds_left: 0 }].into_iter().chain(TrialEnd::ALL.into_iter().map(TrialState::Ended))
            {
                let s = Snapshot { status, outputs: vec![], trial };
                assert_eq!(decode_snapshot(&encode_snapshot(&s)), Ok(s.clone()), "{s:?}");
            }
        }
        // An output with no mode at all keeps its empty last field.
        let mut bare = sample();
        bare.outputs[2].modes.clear();
        assert_eq!(decode_snapshot(&encode_snapshot(&bare)), Ok(bare));
    }

    #[test]
    fn the_wire_text_is_the_documented_one() {
        let text = encode_snapshot(&sample());
        assert_eq!(
            text.lines().take(4).collect::<Vec<_>>(),
            [
                "status ready",
                "trial open 12",
                "output\teDP-1\tLaptop panel\tLaptop panel (compositor)\t0\t2560x1600@165 0 0 1250 0\t1920x1080@60,1280x720@60",
                "output\tDVI-I-1\tLenovo LEN T24i\tLenovo LEN T24i (compositor)\t1\t1920x1080@60 -1920 120 1000 3\t1920x1080@60,1280x720@60",
            ]
        );
        assert!(text.contains("\toff\t1920x1080@60,1280x720@60\n"));
    }

    #[test]
    fn control_characters_never_reach_the_wire() {
        let mut s = sample();
        s.outputs[0].name = "DP-1\t1\nstatus off".into();
        s.outputs[0].description = "evil\x1b[2J\r\ndesc".into();
        s.outputs[0].compositor_description = "a\tb".into();
        let text = encode_snapshot(&s);
        assert_eq!(text.lines().count(), 5, "one line per output, nothing injected: {text:?}");
        assert!(text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'), "{text:?}");
        let back = decode_snapshot(&text).unwrap();
        assert_eq!(back.outputs[0].name, "DP-1 1 status off");
        assert_eq!(back.outputs[0].description, "evil [2J  desc");
        assert_eq!(back.outputs[0].compositor_description, "a b");
        let steps = [Step { output: "DP-1\n\tx".into(), active: None }];
        assert_eq!(decode_desired(&encode_desired(&steps)), Ok(vec![Step { output: "DP-1  x".into(), active: None }]));
        // What is read is checked too: a control character that was never cleaned is refused.
        assert!(decode_snapshot("status ready\ntrial none\noutput\ta\x1b\tb\tc\t0\toff\t\n").is_err());
    }

    #[test]
    fn a_wrong_field_count_is_refused() {
        let ok = "status ready\ntrial none\noutput\tA\tB\tC\t0\toff\t1920x1080@60\n";
        assert!(decode_snapshot(ok).is_ok());
        for bad in [
            "status ready\ntrial none\noutput\tA\tB\tC\t0\toff\n",
            "status ready\ntrial none\noutput\tA\tB\tC\t0\toff\t1920x1080@60\textra\n",
            "status ready\ntrial none\noutput\tA\tB\tC\t2\toff\t\n",
            "status ready\ntrial none\noutput\tA\tB\tC\t0\t1920x1080@60 0 0 1000\t\n",
            "status ready\ntrial none\noutput\tA\tB\tC\t0\t1920x1080@60 0 0 1000 0 9\t\n",
            "status ready\ntrial none\noutput\tA\tB\tC\t0\toff\tnot-a-mode\n",
            "status ready\ntrial none\nsomething\tA\tB\tC\t0\toff\t\n",
            "status ready\ntrial none\n\n",
            "status ready\n",
            "",
            "status maybe\ntrial none\n",
            "trial none\nstatus ready\n",
            "status ready\ntrial open\n",
            "status ready\ntrial open x\n",
            "status ready\ntrial ended nobody\n",
            "status ready\ntrial later\n",
        ] {
            assert!(decode_snapshot(bad).is_err(), "{bad:?}");
        }
        assert!(decode_desired("step\tA\n").is_err());
        assert!(decode_desired("step\tA\toff\tx\n").is_err());
        assert!(decode_desired("output\tA\toff\n").is_err());
        assert!(decode_desired("step\tA\t1920x1080@60 0 0 1000\n").is_err());
    }

    #[test]
    fn desired_round_trips() {
        let steps = vec![
            Step { output: "DVI-I-1".into(), active: Some(active(mode(1920, 1080, 60), -1920, 0, 1000, 0)) },
            Step { output: "eDP-1".into(), active: Some(active(mode(2560, 1600, 165), 0, 0, 1250, 4)) },
            Step { output: "HDMI-A-1".into(), active: None },
        ];
        assert_eq!(decode_desired(&encode_desired(&steps)), Ok(steps));
        assert_eq!(decode_desired(&encode_desired(&[])), Ok(vec![]));
    }

    #[test]
    fn the_body_is_capped() {
        // Counts: encoding cuts, decoding refuses.
        let mut big = sample();
        big.outputs = (0..40).map(|i| output(&format!("DP-{i}"), "x", None, false)).collect();
        big.outputs.iter_mut().for_each(|o| o.modes = vec![mode(800, 600, 60); 300]);
        let text = encode_snapshot(&big);
        assert!(text.len() <= MAX_OUTPUTS_BODY, "{} bytes", text.len());
        let back = decode_snapshot(&text).unwrap();
        assert!(back.outputs.len() <= MAX_OUTPUTS && back.outputs.iter().all(|o| o.modes.len() == MAX_MODES));
        let line = |modes: usize| format!("output\tA\tB\tC\t0\toff\t{}\n", vec!["800x600@60"; modes].join(","));
        assert!(decode_snapshot(&format!("status ready\ntrial none\n{}", line(MAX_MODES))).is_ok());
        assert!(decode_snapshot(&format!("status ready\ntrial none\n{}", line(MAX_MODES + 1))).is_err());
        let many = |n: usize| format!("status ready\ntrial none\n{}", "output\tA\tB\tC\t0\toff\t\n".repeat(n));
        assert!(decode_snapshot(&many(MAX_OUTPUTS)).is_ok());
        assert!(decode_snapshot(&many(MAX_OUTPUTS + 1)).is_err());
        // Bytes: a body past the cap is refused whatever it holds.
        let huge = format!("status ready\ntrial none\n{}", "#".repeat(MAX_OUTPUTS_BODY));
        assert!(decode_snapshot(&huge).is_err());
        assert!(decode_desired(&"#".repeat(MAX_OUTPUTS_BODY + 1)).is_err());
        // Otherwise well-formed, but one byte too long: only the size refuses it.
        let name = "n".repeat(MAX_OUTPUTS_BODY);
        assert!(decode_snapshot(&format!("status ready\ntrial none\noutput\t{name}\tB\tC\t0\toff\t\n")).is_err());
        assert!(decode_desired(&format!("step\t{name}\toff\n")).is_err());
        let steps: Vec<Step> = (0..40).map(|i| Step { output: format!("DP-{i}"), active: None }).collect();
        assert_eq!(decode_desired(&encode_desired(&steps)).unwrap().len(), MAX_OUTPUTS);
        assert!(decode_desired(&"step\tA\toff\n".repeat(MAX_OUTPUTS + 1)).is_err());
        // A long text field is cut.
        let mut s = sample();
        s.outputs[0].description = "é".repeat(1000);
        assert_eq!(decode_snapshot(&encode_snapshot(&s)).unwrap().outputs[0].description.chars().count(), MAX_TEXT);
    }
}
