//! Fil de travail : interroge les backends hors de la boucle d'affichage.
//! Un outil lent (jusqu'à 2 s avant d'être tué) ne fige donc jamais l'écran.
//! Chaque domaine est renvoyé dès qu'il est lu ; une rafale de réglages de
//! curseur est fusionnée (seul le dernier compte).

use super::action::Tab;
use crate::backends::{AudioStatus, BResult, Backends, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, Volume};
use crate::displays::{Output, Plan};
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    Volume,
    Brightness,
    Dnd,
    KeepAwake,
    Profiles,
    Network,
    Bluetooth,
    Audio,
    Displays,
}

impl Domain {
    pub const ALL: [Domain; 9] = [
        Domain::Volume, Domain::Brightness, Domain::Dnd, Domain::KeepAwake,
        Domain::Profiles, Domain::Network, Domain::Bluetooth, Domain::Audio, Domain::Displays,
    ];
}

/// Domaines affichés par un onglet (les plus lents en dernier).
pub fn domains(tab: Tab) -> &'static [Domain] {
    match tab {
        Tab::Accueil => &[
            Domain::Volume, Domain::Brightness, Domain::Dnd, Domain::KeepAwake,
            Domain::Profiles, Domain::Network, Domain::Bluetooth,
        ],
        Tab::Son => &[Domain::Audio],
        Tab::Reseau => &[Domain::Network],
        Tab::Bluetooth => &[Domain::Bluetooth],
        Tab::Alimentation => &[],
        Tab::Ecrans => &[Domain::Displays],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    SetVolume(NodeRef, u8),
    SetDefault(u32),
    SetBrightness(u8),
    SetWifi(bool),
    SetBluetooth(bool),
    SetDnd(bool),
    /// Bascule « Garder éveillé » ; `now_on` est l'état visé, connu au
    /// moment de l'action (le message de réussite ne dépend donc pas d'une
    /// relecture arrivée entre-temps).
    ToggleKeepAwake { now_on: bool },
    SetProfile(String),
    Power(PowerAction),
    /// Réglage d'écrans. `undo` : plan inverse d'un essai (réappliqué si le
    /// plan échoue, ou à l'annulation) ; `None` : c'est lui-même un retour.
    ApplyDisplays { plan: Plan, undo: Option<Plan> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Refresh(Vec<Domain>),
    Do(Op),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Volume(BResult<Volume>),
    Brightness(BResult<u8>),
    Network(BResult<NetStatus>),
    Bluetooth(BResult<BtStatus>),
    Dnd(BResult<bool>),
    KeepAwake(BResult<bool>),
    Profiles(BResult<Profiles>),
    Audio(BResult<AudioStatus>),
    Displays(BResult<Vec<Output>>),
    Done(Op, BResult<()>),
    /// Réglage abandonné par la fusion d'une rafale (un réglage plus récent
    /// du même curseur l'a remplacé) : chaque `Request::Do` reçoit ainsi
    /// exactement une réponse, `Done` ou `Superseded`.
    Superseded(Op),
}

impl Op {
    /// Domaines dont l'affichage est modifié par cette action (affichage
    /// optimiste) : leurs relectures sont ignorées tant qu'elle est en cours.
    pub fn domains(&self) -> &'static [Domain] {
        match self {
            Op::SetVolume(NodeRef::DefaultSink, _) => &[Domain::Volume, Domain::Audio],
            Op::SetVolume(NodeRef::Id(_), _) | Op::SetDefault(_) => &[Domain::Audio, Domain::Volume],
            Op::SetBrightness(_) => &[Domain::Brightness],
            Op::SetWifi(_) => &[Domain::Network],
            Op::SetBluetooth(_) => &[Domain::Bluetooth],
            Op::SetDnd(_) => &[Domain::Dnd],
            Op::ToggleKeepAwake { .. } => &[Domain::KeepAwake],
            Op::SetProfile(_) => &[Domain::Profiles],
            Op::Power(_) => &[],
            Op::ApplyDisplays { .. } => &[Domain::Displays],
        }
    }
}

pub fn fetch(b: &Backends, d: Domain) -> Update {
    match d {
        Domain::Volume => Update::Volume(b.sound.volume()),
        Domain::Brightness => Update::Brightness(b.brightness.get()),
        Domain::Dnd => Update::Dnd(b.notifications.dnd()),
        Domain::KeepAwake => Update::KeepAwake(b.energy.keep_awake()),
        Domain::Profiles => Update::Profiles(b.energy.profiles()),
        Domain::Network => Update::Network(b.network.status()),
        Domain::Bluetooth => Update::Bluetooth(b.bluetooth.status()),
        Domain::Audio => Update::Audio(b.sound.status()),
        Domain::Displays => Update::Displays(b.displays.outputs()),
    }
}

pub fn perform(b: &Backends, op: &Op) -> BResult<()> {
    match op {
        Op::SetVolume(n, v) => b.sound.set_volume(*n, *v),
        Op::SetDefault(id) => b.sound.set_default(*id),
        Op::SetBrightness(v) => b.brightness.set(*v),
        Op::SetWifi(on) => b.network.set_wifi(*on),
        Op::SetBluetooth(on) => b.bluetooth.set_power(*on),
        Op::SetDnd(on) => b.notifications.set_dnd(*on),
        Op::ToggleKeepAwake { .. } => b.energy.toggle_keep_awake(),
        Op::SetProfile(p) => b.energy.set_profile(p),
        Op::Power(a) => b.power.perform(*a),
        Op::ApplyDisplays { plan, .. } => b.displays.apply(plan),
    }
}

/// Fusionne une rafale : dernier réglage par curseur, un seul
/// rafraîchissement (union des domaines) placé après les actions. Renvoie
/// aussi les réglages abandonnés, à annoncer par `Update::Superseded`.
pub fn coalesce(reqs: Vec<Request>) -> (Vec<Request>, Vec<Op>) {
    let mut superseded: Vec<Op> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut refresh: Vec<Domain> = Vec::new();
    for r in reqs {
        match r {
            Request::Refresh(ds) => {
                for d in ds {
                    if !refresh.contains(&d) {
                        refresh.push(d);
                    }
                }
            }
            Request::Do(op) => {
                let replaced = |o: &Op| match (&op, o) {
                    (Op::SetVolume(n, _), Op::SetVolume(m, _)) => m == n,
                    (Op::SetBrightness(_), Op::SetBrightness(_)) => true,
                    _ => false,
                };
                if let Some(i) = ops.iter().position(replaced) {
                    superseded.push(ops.remove(i)); // au plus un : le précédent a déjà été retiré
                }
                ops.push(op);
            }
        }
    }
    let mut out: Vec<Request> = ops.into_iter().map(Request::Do).collect();
    if !refresh.is_empty() {
        out.push(Request::Refresh(refresh));
    }
    (out, superseded)
}

/// Lance le fil ; il s'arrête quand l'émetteur de requêtes est abandonné.
pub fn spawn(b: Backends) -> (Sender<Request>, Receiver<Update>) {
    let (tx_req, rx_req) = mpsc::channel::<Request>();
    let (tx_up, rx_up) = mpsc::channel::<Update>();
    std::thread::spawn(move || {
        while let Ok(first) = rx_req.recv() {
            let mut batch = vec![first];
            while let Ok(r) = rx_req.try_recv() {
                batch.push(r);
            }
            let (reqs, superseded) = coalesce(batch);
            // Une réponse par réglage, même abandonné : l'application compte
            // ses actions en attente et ne doit jamais rester bloquée.
            for op in superseded {
                if tx_up.send(Update::Superseded(op)).is_err() {
                    return;
                }
            }
            for req in reqs {
                let sent = match req {
                    Request::Refresh(ds) => ds.into_iter().all(|d| tx_up.send(fetch(&b, d)).is_ok()),
                    Request::Do(op) => {
                        let r = perform(&b, &op);
                        tx_up.send(Update::Done(op, r)).is_ok()
                    }
                };
                if !sent {
                    return; // l'application est partie
                }
            }
        }
    });
    (tx_req, rx_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn backends(f: &Arc<FakeExecutor>) -> Backends {
        Backends::system(f.clone(), std::env::temp_dir().join("alpheus-worker-sans-drapeau"))
    }

    #[test]
    fn domaines_par_onglet() {
        assert_eq!(domains(Tab::Accueil).len(), 7);
        assert_eq!(domains(Tab::Son), &[Domain::Audio]);
        assert_eq!(domains(Tab::Ecrans), &[Domain::Displays]);
        assert!(domains(Tab::Alimentation).is_empty());
    }

    #[test]
    fn fetch_et_perform() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-status", "on\n");
        f.ok("nmcli radio wifi off", "");
        let b = backends(&f);
        assert_eq!(fetch(&b, Domain::Dnd), Update::Dnd(Ok(true)));
        assert_eq!(perform(&b, &Op::SetWifi(false)), Ok(()));
        assert_eq!(f.calls(), vec!["shell-ctl dnd-status", "nmcli radio wifi off"]);
    }

    #[test]
    fn fusion_des_rafales() {
        let (r, abandonnes) = coalesce(vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 10)),
            Request::Refresh(vec![Domain::Volume]),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
            Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
            Request::Do(Op::SetBrightness(5)),
            Request::Do(Op::SetBrightness(9)),
            Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
        ]);
        assert_eq!(r, vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
            Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
            Request::Do(Op::SetBrightness(9)),
            Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
        ]);
        assert_eq!(abandonnes, vec![Op::SetVolume(NodeRef::DefaultSink, 10), Op::SetBrightness(5)]);
    }

    #[test]
    fn ecrans_lus_et_appliques_par_wm_set() {
        use crate::displays::Step;
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set outputs", "[]");
        f.ok("wm-set monitor-disable eDP-1", "");
        let b = backends(&f);
        assert_eq!(fetch(&b, Domain::Displays), Update::Displays(Ok(Vec::new())));
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] };
        assert_eq!(perform(&b, &Op::ApplyDisplays { plan, undo: None }), Ok(()));
        assert_eq!(f.calls(), vec!["wm-set outputs", "wm-set monitor-disable eDP-1"]);
    }

    #[test]
    fn les_reglages_d_ecran_ne_sont_jamais_fusionnes() {
        // Un essai puis son retour : les deux passent, dans l'ordre.
        let essai = Op::ApplyDisplays { plan: Plan::default(), undo: Some(Plan::default()) };
        let retour = Op::ApplyDisplays { plan: Plan::default(), undo: None };
        let (r, abandonnes) = coalesce(vec![Request::Do(essai.clone()), Request::Do(retour.clone())]);
        assert_eq!(r, vec![Request::Do(essai), Request::Do(retour)]);
        assert!(abandonnes.is_empty());
    }

    #[test]
    fn les_bascules_ne_sont_jamais_fusionnees() {
        let (r, abandonnes) = coalesce(vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::SetWifi(true))]);
        assert_eq!(r.len(), 2);
        assert!(abandonnes.is_empty());
    }

    #[test]
    fn le_fil_repond() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-status", "off\n");
        f.ok("shell-ctl dnd-on", "");
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Dnd, Domain::KeepAwake])).unwrap();
        let t = Duration::from_secs(1);
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::Dnd(Ok(false)));
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::KeepAwake(Ok(false)));
        tx.send(Request::Do(Op::SetDnd(true))).unwrap();
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::Done(Op::SetDnd(true), Ok(())));
    }

    #[test]
    fn chaque_reglage_recoit_une_reponse_meme_fusionne() {
        // Le fil est occupé 200 ms par une lecture lente : les trois
        // réglages s'accumulent derrière et forment une seule rafale.
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(200)));
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Dnd])).unwrap();
        // L'appel est noté avant l'attente : dès qu'il paraît, le fil est
        // occupé et sa rafale courante déjà formée.
        let t0 = Instant::now();
        while f.calls().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "le fil n'a pas démarré");
            std::thread::yield_now();
        }
        tx.send(Request::Do(Op::SetBrightness(30))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(40))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(50))).unwrap();
        let t = Duration::from_secs(2);
        assert!(matches!(rx.recv_timeout(t).unwrap(), Update::Dnd(_)));
        let mut recus = Vec::new();
        while recus.len() < 3 {
            recus.push(rx.recv_timeout(t).expect("une réponse par réglage"));
        }
        let fin = recus.pop().unwrap();
        assert!(matches!(fin, Update::Done(Op::SetBrightness(50), _)), "{fin:?}");
        assert_eq!(recus, vec![Update::Superseded(Op::SetBrightness(30)), Update::Superseded(Op::SetBrightness(40))]);
    }

    #[test]
    fn domaines_touches_par_une_action() {
        assert_eq!(Op::SetWifi(true).domains(), &[Domain::Network]);
        assert_eq!(Op::ToggleKeepAwake { now_on: true }.domains(), &[Domain::KeepAwake]);
        assert!(Op::SetVolume(NodeRef::DefaultSink, 5).domains().contains(&Domain::Audio));
        assert!(Op::SetDefault(64).domains().contains(&Domain::Volume));
        assert!(Op::Power(crate::backends::PowerAction::Lock).domains().is_empty());
        assert_eq!(Op::ApplyDisplays { plan: Plan::default(), undo: None }.domains(), &[Domain::Displays]);
    }

    #[test]
    fn spawn_ne_bloque_pas_l_appelant() {
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(300)));
        let (tx, rx) = spawn(backends(&f));
        let t = Instant::now();
        tx.send(Request::Refresh(domains(Tab::Accueil).to_vec())).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50));
        // Le premier domaine arrive sans attendre les six autres.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Update::Volume(Err(_))));
    }
}
