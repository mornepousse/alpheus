//! Écrans : lecture par `wm-set outputs` (JSON normalisé), écriture par
//! `wm-set monitor` / `monitor-disable`. Jamais le compositeur en direct.

use super::{run_ok, unparsable, BResult, BackendError};
use crate::displays::{Active, Mode, Output, Plan, Scale, Step};
use crate::exec::Executor;
use serde::Deserialize;
use std::sync::Arc;

const TOOL: &str = "wm-set";

pub trait Displays: Send + Sync {
    /// Sorties connues du compositeur, activées ou non.
    fn outputs(&self) -> BResult<Vec<Output>>;
    /// Exécute le plan dans l'ordre ; s'arrête au premier échec (essai :
    /// jamais à moitié, l'échec déclenche aussitôt le retour).
    fn apply(&self, plan: &Plan) -> BResult<()>;
    /// Exécute le plan dans l'ordre, chaque étape même si une précédente a
    /// échoué (retour : rallumer tout ce qui peut l'être) ; l'erreur
    /// `Incomplete` résume les sorties en échec.
    fn apply_best_effort(&self, plan: &Plan) -> BResult<()>;
}

pub struct WmSetDisplays {
    exec: Arc<dyn Executor>,
}

impl WmSetDisplays {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        WmSetDisplays { exec }
    }
}

impl Displays for WmSetDisplays {
    fn outputs(&self) -> BResult<Vec<Output>> {
        parse_outputs(&run_ok(&*self.exec, TOOL, &["outputs"])?)
    }

    fn apply(&self, plan: &Plan) -> BResult<()> {
        plan.steps.iter().try_for_each(|s| self.run_step(s))
    }

    fn apply_best_effort(&self, plan: &Plan) -> BResult<()> {
        let failures: Vec<(String, BackendError)> = plan
            .steps
            .iter()
            .filter_map(|s| self.run_step(s).err().map(|e| (s.output.clone(), e)))
            .collect();
        if failures.is_empty() { Ok(()) } else { Err(BackendError::Incomplete { failures }) }
    }
}

impl WmSetDisplays {
    fn run_step(&self, step: &Step) -> BResult<()> {
        let args = step.args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(&*self.exec, TOOL, &args).map(drop)
    }
}

/// Une entrée de `wm-set outputs` telle qu'écrite par le script.
#[derive(Deserialize)]
struct RawOutput {
    name: String,
    description: String,
    enabled: bool,
    mode: String,
    modes: Vec<String>,
    x: i32,
    y: i32,
    scale: f64,
    transform: u8,
    displaylink: bool,
}

/// Analyse le JSON de `wm-set outputs`. Un mode de la liste illisible est
/// ignoré ; le mode courant illisible d'une sortie ACTIVÉE rend tout
/// illisible (on ne devine pas l'état d'un écran allumé).
pub fn parse_outputs(json: &str) -> BResult<Vec<Output>> {
    let raw: Vec<RawOutput> = serde_json::from_str(json).map_err(|e| unparsable(TOOL, e.to_string()))?;
    raw.into_iter()
        .map(|r| {
            let active = if r.enabled {
                let mode = Mode::parse(&r.mode).ok_or_else(|| unparsable(TOOL, format!("mode « {} » de {}", r.mode, r.name)))?;
                let scale = Scale::from_f64(r.scale).ok_or_else(|| unparsable(TOOL, format!("échelle {} de {}", r.scale, r.name)))?;
                if r.transform > 7 {
                    return Err(unparsable(TOOL, format!("rotation {} de {}", r.transform, r.name)));
                }
                Some(Active { mode, x: r.x, y: r.y, scale, transform: r.transform })
            } else {
                None
            };
            Ok(Output {
                modes: r.modes.iter().filter_map(|m| Mode::parse(m)).collect(),
                name: r.name,
                description: r.description,
                displaylink: r.displaylink,
                active,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::testing::*;
    use crate::exec::fake::FakeExecutor;
    use crate::exec::Output as ExecOutput;

    /// Relevé `wm-set outputs` du poste, anonymisé (descriptions remplacées).
    const OUTPUTS: &str = include_str!("../../tests/fixtures/wm-set-outputs.json");

    #[test]
    fn analyse_le_releve_du_poste() {
        let o = parse_outputs(OUTPUTS).unwrap();
        assert_eq!(o.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["eDP-1", "DVI-I-1", "HDMI-A-1"]);
        assert_eq!(o[1].description, ALPHA);
        assert!(o[1].displaylink && !o[0].displaylink && !o[2].displaylink);
        assert_eq!(o[1].active, Some(Active { mode: mode(1920, 1080, 60), x: 213, y: 0, scale: Scale::ONE, transform: 0 }));
        assert_eq!(o[2].active.as_ref().unwrap().mode, mode(1920, 1080, 240));
        assert_eq!(o[0].active.as_ref().map(|a| (a.x, a.y)), Some((0, 1080)));
        assert_eq!(o[1].modes.len(), 26);
        assert_eq!(&o[2].modes[..3], &[mode(1920, 1080, 60), mode(1920, 1080, 240), mode(1920, 1080, 120)]);
        // Même agencement que l'aide aux tests `poste()`.
        assert_eq!(o.iter().map(|x| x.active.clone()).collect::<Vec<_>>(), poste().into_iter().map(|x| x.active).collect::<Vec<_>>());
    }

    #[test]
    fn sortie_desactivee_et_modes_illisibles() {
        let j = r#"[{"name":"DP-3","description":"Charlie Modèle 3 SERIE0003","enabled":false,"mode":"0x0@0",
            "modes":["3840x2160@60","bof"],"x":0,"y":0,"scale":1.00,"transform":0,"displaylink":false}]"#;
        let o = parse_outputs(j).unwrap();
        assert_eq!(o[0].active, None);
        assert_eq!(o[0].modes, vec![mode(3840, 2160, 60)]);
    }

    #[test]
    fn sortie_illisible() {
        let actif_sans_mode = r#"[{"name":"DP-3","description":"C","enabled":true,"mode":"0x0@0","modes":[],
            "x":0,"y":0,"scale":1,"transform":0,"displaylink":false}]"#;
        for j in ["", "{}", "[{\"name\": 3}]", actif_sans_mode] {
            assert!(matches!(parse_outputs(j), Err(BackendError::Unparsable { .. })), "{j}");
        }
    }

    #[test]
    fn lecture_par_wm_set_outputs() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set outputs", OUTPUTS);
        let d = WmSetDisplays::new(f.clone());
        assert_eq!(d.outputs().unwrap().len(), 3);
        assert_eq!(f.calls(), vec!["wm-set outputs"]);
    }

    #[test]
    fn wm_set_absent_ou_ancien() {
        let d = WmSetDisplays::new(Arc::new(FakeExecutor::new()));
        assert_eq!(d.outputs(), Err(BackendError::Missing { tool: "wm-set".into() }));
        let f = Arc::new(FakeExecutor::new());
        f.respond("wm-set outputs", Ok(ExecOutput::fail(1, "sous-commande inconnue : outputs\n")));
        let e = WmSetDisplays::new(f).outputs().unwrap_err();
        assert_eq!(e.to_string(), "wm-set a échoué (code 1) : sous-commande inconnue : outputs");
    }

    fn plan3() -> Plan {
        let a = |x| Some(Active { mode: mode(1920, 1080, 60), x, y: 0, scale: Scale(1250), transform: 0 });
        Plan { steps: vec![
            Step { output: "DVI-I-1".into(), active: a(0) },
            Step { output: "HDMI-A-1".into(), active: a(1536) },
            Step { output: "eDP-1".into(), active: None },
        ] }
    }

    #[test]
    fn lignes_de_commande_exactes() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0", "");
        f.ok("wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0", "");
        f.ok("wm-set monitor-disable eDP-1", "");
        WmSetDisplays::new(f.clone()).apply(&plan3()).unwrap();
        assert_eq!(f.calls(), vec![
            "wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0",
            "wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0",
            "wm-set monitor-disable eDP-1",
        ]);
    }

    #[test]
    fn arret_au_premier_echec() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0", "");
        f.respond("wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0", Ok(ExecOutput::fail(1, "Hyprland a refusé « eval … » : erreur\n")));
        f.ok("wm-set monitor-disable eDP-1", "");
        let e = WmSetDisplays::new(f.clone()).apply(&plan3()).unwrap_err();
        assert_eq!(e.to_string(), "wm-set a échoué (code 1) : Hyprland a refusé « eval … » : erreur");
        assert_eq!(f.calls().len(), 2, "la désactivation n'est jamais lancée : {:?}", f.calls());
    }

    #[test]
    fn retour_au_mieux_dalle_interne_d_abord() {
        // Décision D2 : un retour applique chaque étape même si une autre
        // échoue, la dalle interne en premier, la DisplayLink en dernier.
        let retour = Plan::undo(&poste(), &plan3());
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor eDP-1 2560x1600@60 0x1080 1 0", "");
        f.ok("wm-set monitor HDMI-A-1 1920x1080@240 2133x0 1 0", "");
        f.respond("wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0", Ok(ExecOutput::fail(1, "gel du dock\n")));
        let e = WmSetDisplays::new(f.clone()).apply_best_effort(&retour).unwrap_err();
        assert_eq!(f.calls(), vec![
            "wm-set monitor eDP-1 2560x1600@60 0x1080 1 0",
            "wm-set monitor HDMI-A-1 1920x1080@240 2133x0 1 0",
            "wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0",
        ]);
        assert_eq!(e.to_string(), "retour incomplet : DVI-I-1 (wm-set a échoué (code 1) : gel du dock)");

        // Les premières étapes en échec n'arrêtent pas les suivantes.
        let f = Arc::new(FakeExecutor::new());
        f.respond("wm-set monitor eDP-1 2560x1600@60 0x1080 1 0", Ok(ExecOutput::fail(1, "refusé\n")));
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0", "");
        // HDMI-A-1 non programmé : « wm-set introuvable ».
        let e = WmSetDisplays::new(f.clone()).apply_best_effort(&retour).unwrap_err();
        assert_eq!(f.calls().len(), 3, "toutes les étapes tentées : {:?}", f.calls());
        assert_eq!(e.to_string(), "retour incomplet : eDP-1 (wm-set a échoué (code 1) : refusé), HDMI-A-1 (wm-set introuvable)");

        // Tout passe : Ok.
        let f = Arc::new(FakeExecutor::new());
        for c in ["eDP-1 2560x1600@60 0x1080", "HDMI-A-1 1920x1080@240 2133x0", "DVI-I-1 1920x1080@60 213x0"] {
            f.ok(&format!("wm-set monitor {c} 1 0"), "");
        }
        assert_eq!(WmSetDisplays::new(f.clone()).apply_best_effort(&retour), Ok(()));
        assert_eq!(f.calls().len(), 3);
    }
}
