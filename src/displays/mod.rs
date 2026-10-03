//! Écrans : types communs (mode, échelle, réglage d'une sortie) et plans
//! d'application (suite d'appels `wm-set`). Aucun appel ici : le backend
//! (`backends::displays`) exécute les plans, la géométrie et les profils
//! les calculent.

use std::fmt;

/// Mode d'affichage, fréquence arrondie à l'entier comme dans `wm-set
/// outputs` : « 1920x1080@60 ».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
}

impl Mode {
    /// « 1920x1080@60 » ; `None` si illisible ou nul (une sortie désactivée
    /// peut rapporter « 0x0@0 »).
    pub fn parse(s: &str) -> Option<Mode> {
        let (size, hz) = s.trim().split_once('@')?;
        let (w, h) = size.split_once('x')?;
        let m = Mode { width: w.parse().ok()?, height: h.parse().ok()?, refresh: hz.parse().ok()? };
        (m.width > 0 && m.height > 0 && m.refresh > 0).then_some(m)
    }

    /// Libellé affiché : « 1920×1080@60 ».
    pub fn label(&self) -> String {
        format!("{}×{}@{}", self.width, self.height, self.refresh)
    }
}

/// Forme attendue par `wm-set monitor` : « 1920x1080@60 ».
impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh)
    }
}

/// Échelle en millièmes (1,25 → 1250) : comparable exactement, sans flottant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scale(pub u32);

impl Scale {
    pub const ONE: Scale = Scale(1000);
    /// Bornes et pas du panneau de réglage : de 0,5 à 3 par 0,25.
    pub const MIN: Scale = Scale(500);
    pub const MAX: Scale = Scale(3000);
    pub const STEP: u32 = 250;

    /// Depuis la valeur de Hyprland (1.0, 1.25, 1.333333…).
    pub fn from_f64(v: f64) -> Option<Scale> {
        if !v.is_finite() || v <= 0.0 || v > 10.0 {
            return None;
        }
        let m = (v * 1000.0).round() as u32;
        (m > 0).then_some(Scale(m))
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    /// Argument de `wm-set` : « 1 », « 1.25 », « 1.2 ».
    pub fn arg(self) -> String {
        let (whole, frac) = (self.0 / 1000, self.0 % 1000);
        if frac == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{frac:03}").trim_end_matches('0').to_string()
        }
    }

    /// Libellé affiché : « 1.0 », « 1.25 ».
    pub fn label(self) -> String {
        if self.0 % 1000 == 0 { format!("{}.0", self.0 / 1000) } else { self.arg() }
    }

    /// Pas suivant (`dir` > 0) ou précédent, borné à [0,5 ; 3]. Une échelle
    /// hors grille (1,2) se cale d'abord sur la grille (1,25 ou 1,0).
    pub fn step(self, dir: i32) -> Scale {
        let s = self.0;
        let next = if dir > 0 {
            (s / Self::STEP + 1) * Self::STEP
        } else if s % Self::STEP == 0 {
            s.saturating_sub(Self::STEP)
        } else {
            s / Self::STEP * Self::STEP
        };
        Scale(next.clamp(Self::MIN.0, Self::MAX.0))
    }
}

/// Rotation Hyprland (0-7, `wl_output_transform`) : « 0° », « 90° »… ;
/// 4 à 7 sont les variantes retournées.
pub fn rotation_label(t: u8) -> String {
    let deg = u32::from(t % 4) * 90;
    if t >= 4 { format!("retourné {deg}°") } else { format!("{deg}°") }
}

/// Réglage d'une sortie activée (coordonnées logiques, comme Hyprland).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub mode: Mode,
    pub x: i32,
    pub y: i32,
    pub scale: Scale,
    pub transform: u8,
}

/// Une sortie connue du compositeur (`wm-set outputs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Connecteur : « DVI-I-1 » (peut changer d'un branchement à l'autre).
    pub name: String,
    /// Marque, modèle, numéro de série : stable, sert d'identifiant.
    pub description: String,
    /// Modes réels, dans l'ordre de Hyprland.
    pub modes: Vec<Mode>,
    pub displaylink: bool,
    /// `None` : sortie désactivée.
    pub active: Option<Active>,
}

/// Nom court tiré d'une description : son premier mot (la marque).
pub fn short_from_description(desc: &str) -> String {
    desc.split_whitespace().next().unwrap_or("?").to_string()
}

fn short_name(o: &Output) -> String {
    if o.name.starts_with("eDP") {
        "Portable".to_string()
    } else if o.description.trim().is_empty() {
        o.name.clone()
    } else {
        short_from_description(&o.description)
    }
}

/// Noms courts affichés, dans l'ordre des sorties : « Portable » pour une
/// dalle interne (eDP-*), sinon la marque. Deux sorties de même marque se
/// distinguent par leur connecteur (« Bravo HDMI-A-1 »).
pub fn short_names(outs: &[Output]) -> Vec<String> {
    let base: Vec<String> = outs.iter().map(short_name).collect();
    base.iter()
        .zip(outs)
        .map(|(b, o)| {
            if base.iter().filter(|x| *x == b).count() > 1 { format!("{b} {}", o.name) } else { b.clone() }
        })
        .collect()
}

/// Nombre de sorties activées.
pub fn active_count(outs: &[Output]) -> usize {
    outs.iter().filter(|o| o.active.is_some()).count()
}

/// Un appel `wm-set` : régler (`Some`) ou désactiver (`None`) une sortie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub output: String,
    pub active: Option<Active>,
}

impl Step {
    /// Arguments de `wm-set` : `monitor <sortie> <mode> <XxY> <échelle>
    /// <rotation>` ou `monitor-disable <sortie>`.
    pub fn args(&self) -> Vec<String> {
        match &self.active {
            Some(a) => vec![
                "monitor".into(),
                self.output.clone(),
                a.mode.to_string(),
                format!("{}x{}", a.x, a.y),
                a.scale.arg(),
                a.transform.to_string(),
            ],
            None => vec!["monitor-disable".into(), self.output.clone()],
        }
    }
}

/// Suite d'appels `wm-set`, exécutée dans l'ordre et arrêtée au premier échec.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
}

/// Activations d'abord, désactivations ensuite : jamais d'instant sans écran.
fn ordered(mut steps: Vec<Step>) -> Plan {
    steps.sort_by_key(|s| s.active.is_none()); // tri stable : l'ordre est gardé
    Plan { steps }
}

impl Plan {
    /// Étapes pour passer de `current` aux réglages `target` (par nom de
    /// sortie) : seules les sorties qui changent ; une sortie inconnue est
    /// ignorée.
    pub fn between(current: &[Output], target: &[(String, Option<Active>)]) -> Plan {
        let steps = target
            .iter()
            .filter(|(name, t)| current.iter().any(|o| o.name == *name && o.active != *t))
            .map(|(name, t)| Step { output: name.clone(), active: t.clone() })
            .collect();
        ordered(steps)
    }

    /// Plan qui rend aux sorties touchées par `plan` leur réglage de `before`.
    pub fn undo(before: &[Output], plan: &Plan) -> Plan {
        let steps = plan
            .steps
            .iter()
            .filter_map(|s| before.iter().find(|o| o.name == s.output))
            .map(|o| Step { output: o.name.clone(), active: o.active.clone() })
            .collect();
        ordered(steps)
    }

    pub fn touches(&self, output: &str) -> bool {
        self.steps.iter().any(|s| s.output == output)
    }
}

/// Les écrans ont-ils changé hors d'alpheus pendant un essai ? Oui si une
/// sortie est apparue ou a disparu (dock), ou si une sortie que le plan ne
/// touche pas a changé de réglage (monitor-toggle…). Les sorties touchées ne
/// comptent pas : Hyprland peut les régler à peine autrement que demandé.
pub fn changed_elsewhere(before: &[Output], plan: &Plan, now: &[Output]) -> bool {
    let names = |v: &[Output]| {
        let mut n: Vec<String> = v.iter().map(|o| o.name.clone()).collect();
        n.sort();
        n
    };
    if names(before) != names(now) {
        return true;
    }
    now.iter()
        .filter(|o| !plan.touches(&o.name))
        .any(|o| before.iter().any(|b| b.name == o.name && b.active != o.active))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn mode(w: u32, h: u32, r: u32) -> Mode {
        Mode { width: w, height: h, refresh: r }
    }

    /// Sortie activée, échelle 1, sans rotation ; son mode est aussi le seul de sa liste.
    pub fn on(name: &str, desc: &str, w: u32, h: u32, x: i32, y: i32) -> Output {
        let m = mode(w, h, 60);
        Output {
            name: name.into(),
            description: desc.into(),
            modes: vec![m],
            displaylink: false,
            active: Some(Active { mode: m, x, y, scale: Scale::ONE, transform: 0 }),
        }
    }

    pub fn off(name: &str, desc: &str) -> Output {
        Output { name: name.into(), description: desc.into(), modes: vec![mode(1920, 1080, 60)], displaylink: false, active: None }
    }

    pub const INTERNE: &str = "Interne Dalle 0x0001";
    pub const ALPHA: &str = "Alpha Modèle 1 SERIE0001";
    pub const BRAVO: &str = "Bravo Modèle 2 SERIE0002";

    /// Agencement réel du poste (relevé anonymisé) : Alpha (DisplayLink) en
    /// haut à 213, Bravo (240 Hz) à sa droite, la dalle interne en dessous, à x = 0.
    pub fn poste() -> Vec<Output> {
        let mut alpha = on("DVI-I-1", ALPHA, 1920, 1080, 213, 0);
        alpha.displaylink = true;
        let mut bravo = on("HDMI-A-1", BRAVO, 1920, 1080, 2133, 0);
        bravo.modes = vec![mode(1920, 1080, 60), mode(1920, 1080, 240)];
        if let Some(a) = &mut bravo.active {
            a.mode = mode(1920, 1080, 240);
        }
        vec![on("eDP-1", INTERNE, 2560, 1600, 0, 1080), alpha, bravo]
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(Mode::parse("1920x1080@60"), Some(mode(1920, 1080, 60)));
        assert_eq!(Mode::parse(" 2560x1600@60\n"), Some(mode(2560, 1600, 60)));
        assert_eq!(Mode::parse("0x0@0"), None, "sortie désactivée");
        assert_eq!(Mode::parse("1920x1080"), None);
        assert_eq!(Mode::parse("1920x1080@59.93"), None, "déjà arrondi par wm-set");
        assert_eq!(mode(1920, 1080, 60).to_string(), "1920x1080@60");
        assert_eq!(mode(1920, 1080, 60).label(), "1920×1080@60");
    }

    #[test]
    fn echelles() {
        assert_eq!(Scale::from_f64(1.0), Some(Scale::ONE));
        assert_eq!(Scale::from_f64(1.333333), Some(Scale(1333)));
        assert_eq!(Scale::from_f64(0.0), None);
        assert_eq!(Scale::from_f64(f64::NAN), None);
        assert_eq!(Scale(1000).arg(), "1");
        assert_eq!(Scale(1250).arg(), "1.25");
        assert_eq!(Scale(1200).arg(), "1.2");
        assert_eq!(Scale(1000).label(), "1.0");
        assert_eq!(Scale(1500).label(), "1.5");
    }

    #[test]
    fn pas_d_echelle_bornes_et_cales_sur_la_grille() {
        assert_eq!(Scale(1000).step(1), Scale(1250));
        assert_eq!(Scale(1000).step(-1), Scale(750));
        assert_eq!(Scale(1200).step(1), Scale(1250), "hors grille : vers le cran suivant");
        assert_eq!(Scale(1200).step(-1), Scale(1000), "hors grille : vers le cran précédent");
        assert_eq!(Scale(500).step(-1), Scale(500), "plancher 0,5");
        assert_eq!(Scale(3000).step(1), Scale(3000), "plafond 3");
    }

    #[test]
    fn rotations() {
        assert_eq!(rotation_label(0), "0°");
        assert_eq!(rotation_label(3), "270°");
        assert_eq!(rotation_label(5), "retourné 90°");
    }

    #[test]
    fn noms_courts() {
        assert_eq!(short_names(&poste()), vec!["Portable", "Alpha", "Bravo"]);
        let deux = vec![on("DVI-I-1", "Bravo Modèle 2 SERIE0002", 1920, 1080, 0, 0), on("DVI-I-2", "Bravo Modèle 2 SERIE0003", 1920, 1080, 1920, 0)];
        assert_eq!(short_names(&deux), vec!["Bravo DVI-I-1", "Bravo DVI-I-2"], "même marque : connecteur ajouté");
        assert_eq!(short_names(&[on("HDMI-A-2", "  ", 1920, 1080, 0, 0)]), vec!["HDMI-A-2"], "sans description");
        assert_eq!(short_from_description(BRAVO), "Bravo");
    }

    #[test]
    fn arguments_de_wm_set() {
        let a = Active { mode: mode(1920, 1080, 60), x: 213, y: 0, scale: Scale(1250), transform: 1 };
        let s = Step { output: "DVI-I-1".into(), active: Some(a) };
        assert_eq!(s.args(), vec!["monitor", "DVI-I-1", "1920x1080@60", "213x0", "1.25", "1"]);
        let s = Step { output: "eDP-1".into(), active: None };
        assert_eq!(s.args(), vec!["monitor-disable", "eDP-1"]);
    }

    #[test]
    fn plan_entre_deux_etats_activations_d_abord() {
        let cur = poste();
        let moved = Active { x: 0, ..cur[1].active.clone().unwrap() };
        let target = vec![
            ("eDP-1".to_string(), None),
            ("DVI-I-1".to_string(), Some(moved.clone())),
            ("HDMI-A-1".to_string(), cur[2].active.clone()), // inchangé : absent du plan
            ("DP-9".to_string(), None),                       // inconnue : ignorée
        ];
        let p = Plan::between(&cur, &target);
        assert_eq!(p.steps, vec![
            Step { output: "DVI-I-1".into(), active: Some(moved) },
            Step { output: "eDP-1".into(), active: None },
        ]);
        assert!(p.touches("eDP-1") && !p.touches("HDMI-A-1"));
    }

    #[test]
    fn plan_inverse() {
        let before = poste();
        let p = Plan { steps: vec![
            Step { output: "HDMI-A-1".into(), active: None },
            Step { output: "eDP-1".into(), active: None },
        ] };
        let u = Plan::undo(&before, &p);
        assert_eq!(u.steps, vec![
            Step { output: "HDMI-A-1".into(), active: before[2].active.clone() },
            Step { output: "eDP-1".into(), active: before[0].active.clone() },
        ]);
        // Une sortie éteinte avant l'essai est rééteinte, après les activations.
        let before = vec![off("DP-3", ALPHA), on("eDP-1", INTERNE, 2560, 1600, 0, 0)];
        let p = Plan { steps: vec![
            Step { output: "DP-3".into(), active: before[1].active.clone() },
            Step { output: "eDP-1".into(), active: None },
        ] };
        let u = Plan::undo(&before, &p);
        assert_eq!(u.steps[0].output, "eDP-1");
        assert_eq!(u.steps[1], Step { output: "DP-3".into(), active: None });
    }

    #[test]
    fn changement_exterieur() {
        let before = poste();
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] };
        let mut now = before.clone();
        now[0].active = None; // effet attendu du plan
        assert!(!changed_elsewhere(&before, &plan, &now));
        now[0].active = Some(Active { scale: Scale(1250), ..before[0].active.clone().unwrap() });
        assert!(!changed_elsewhere(&before, &plan, &now), "sortie touchée : réglée autrement, pas « ailleurs »");
        let mut now2 = now.clone();
        now2[1].active = None; // monitor-toggle a éteint Alpha
        assert!(changed_elsewhere(&before, &plan, &now2));
        let mut now3 = now.clone();
        now3.pop(); // Bravo débranché
        assert!(changed_elsewhere(&before, &plan, &now3));
        let mut now4 = now.clone();
        now4.push(off("DP-3", "Charlie Modèle 3 SERIE0003")); // dock branché
        assert!(changed_elsewhere(&before, &plan, &now4));
    }

    #[test]
    fn compte_des_ecrans_actifs() {
        let mut v = poste();
        assert_eq!(active_count(&v), 3);
        v[0].active = None;
        assert_eq!(active_count(&v), 2);
    }
}
