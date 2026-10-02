//! Fil de travail : interroge les backends hors de la boucle d'affichage.
//! Un outil lent (jusqu'à 2 s avant d'être tué) ne fige donc jamais l'écran.
//! Chaque domaine est renvoyé dès qu'il est lu ; une rafale de réglages de
//! curseur est fusionnée (seul le dernier compte).

use super::action::Tab;
use crate::backends::{AudioStatus, BResult, Backends, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, Volume};
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
}

impl Domain {
    pub const ALL: [Domain; 8] = [
        Domain::Volume, Domain::Brightness, Domain::Dnd, Domain::KeepAwake,
        Domain::Profiles, Domain::Network, Domain::Bluetooth, Domain::Audio,
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
    ToggleKeepAwake,
    SetProfile(String),
    Power(PowerAction),
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
    Done(Op, BResult<()>),
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
        Op::ToggleKeepAwake => b.energy.toggle_keep_awake(),
        Op::SetProfile(p) => b.energy.set_profile(p),
        Op::Power(a) => b.power.perform(*a),
    }
}

/// Fusionne une rafale : dernier réglage par curseur, un seul
/// rafraîchissement (union des domaines) placé après les actions.
pub fn coalesce(reqs: Vec<Request>) -> Vec<Request> {
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
                match &op {
                    Op::SetVolume(n, _) => ops.retain(|o| !matches!(o, Op::SetVolume(m, _) if m == n)),
                    Op::SetBrightness(_) => ops.retain(|o| !matches!(o, Op::SetBrightness(_))),
                    _ => {}
                }
                ops.push(op);
            }
        }
    }
    let mut out: Vec<Request> = ops.into_iter().map(Request::Do).collect();
    if !refresh.is_empty() {
        out.push(Request::Refresh(refresh));
    }
    out
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
            for req in coalesce(batch) {
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
        let r = coalesce(vec![
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
    }

    #[test]
    fn les_bascules_ne_sont_jamais_fusionnees() {
        let r = coalesce(vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::SetWifi(true))]);
        assert_eq!(r.len(), 2);
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
