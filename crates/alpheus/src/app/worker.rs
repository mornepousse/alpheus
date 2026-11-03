//! Worker thread: queries the backends outside the render loop. A slow tool
//! (killed after up to 2 s) therefore never freezes the screen. Each domain
//! is returned as soon as it's read; a burst of slider changes is coalesced
//! (only the last one counts).

use crate::backends::{AudioStatus, BResult, BackendError, Backends, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, Volume};
use crate::displays::Plan;
use crate::displays::wire::Snapshot;
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
        Domain::Volume,
        Domain::Brightness,
        Domain::Dnd,
        Domain::KeepAwake,
        Domain::Profiles,
        Domain::Network,
        Domain::Bluetooth,
        Domain::Audio,
        Domain::Displays,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    SetVolume(NodeRef, u8),
    SetDefault(u32),
    SetBrightness(u8),
    SetWifi(bool),
    SetBluetooth(bool),
    SetDnd(bool),
    /// Toggles "Keep awake"; `now_on` is the state aimed for, known at the
    /// time of the action (so the success message doesn't depend on a
    /// stale read arriving in the meantime).
    ToggleKeepAwake {
        now_on: bool,
    },
    SetProfile(String),
    Power(PowerAction),
    /// Display setting, tried by the daemon: answered once the compositor
    /// has, its 15 s trial then open there.
    TryDisplays(Plan),
    /// Keeps the daemon's trial.
    KeepDisplays,
    /// Reverts the daemon's trial.
    RevertDisplays,
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
    Displays(BResult<Snapshot>),
    Done(Op, BResult<()>),
    /// A setting dropped by coalescing a burst (a more recent change to the
    /// same slider replaced it): each `Request::Do` thus gets exactly one
    /// response, `Done` or `Superseded`.
    Superseded(Op),
}

impl Op {
    /// Domains whose display this action changes (optimistic display):
    /// their reads are ignored while it's in flight.
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
            Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays => &[Domain::Displays],
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
        Domain::Displays => Update::Displays(b.displays.snapshot().map_err(BackendError::Daemon)),
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
        Op::TryDisplays(plan) => b.displays.try_(&plan.steps).map_err(BackendError::Daemon),
        Op::KeepDisplays => b.displays.keep().map_err(BackendError::Daemon),
        Op::RevertDisplays => b.displays.revert().map_err(BackendError::Daemon),
    }
}

/// Coalesces a burst: last setting per slider, a single refresh (union of
/// domains) placed after the actions. Also returns the dropped settings, to
/// be announced as `Update::Superseded`.
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
                    superseded.push(ops.remove(i)); // at most one: the previous one was already removed
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

/// Starts the thread; it stops once the request sender is dropped.
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
            // One response per setting, even a dropped one: the app counts
            // its pending actions and must never stay stuck.
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
                    return; // the app has gone away
                }
            }
        }
    });
    (tx_req, rx_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action::Tab;
    use crate::exec::fake::FakeExecutor;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn backends(f: &Arc<FakeExecutor>) -> Backends {
        Backends::system(f.clone(), std::env::temp_dir().join("alpheus-worker-no-flag"))
    }

    #[test]
    fn domains_per_tab() {
        assert_eq!(Tab::Home.domains().len(), 7);
        assert_eq!(Tab::Sound.domains(), &[Domain::Audio]);
        assert_eq!(Tab::Displays.domains(), &[Domain::Displays]);
        assert!(Tab::Power.domains().is_empty());
        assert!(Tab::Appearance.domains().is_empty());
    }

    #[test]
    fn fetch_and_perform() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-status", "on\n");
        f.ok("nmcli radio wifi off", "");
        let b = backends(&f);
        assert_eq!(fetch(&b, Domain::Dnd), Update::Dnd(Ok(true)));
        assert_eq!(perform(&b, &Op::SetWifi(false)), Ok(()));
        assert_eq!(f.calls(), vec!["shell-ctl dnd-status", "nmcli radio wifi off"]);
    }

    #[test]
    fn burst_coalescing() {
        let (r, dropped) = coalesce(vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 10)),
            Request::Refresh(vec![Domain::Volume]),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
            Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
            Request::Do(Op::SetBrightness(5)),
            Request::Do(Op::SetBrightness(9)),
            Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
        ]);
        assert_eq!(
            r,
            vec![
                Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
                Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
                Request::Do(Op::SetBrightness(9)),
                Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
            ]
        );
        assert_eq!(dropped, vec![Op::SetVolume(NodeRef::DefaultSink, 10), Op::SetBrightness(5)]);
    }

    #[test]
    fn displays_are_read_and_changed_through_the_daemon() {
        use crate::app::testing::FakeDisplays;
        use crate::displays::Step;
        use crate::displays::wire::{Status, TrialState};
        let f = Arc::new(FakeExecutor::new());
        let fake = Arc::new(FakeDisplays::default());
        let mut b = backends(&f);
        b.displays = fake.clone();
        let snapshot = Snapshot { status: Status::Ready, outputs: crate::displays::testing::machine(), trial: TrialState::None };
        *fake.snapshot.lock().unwrap() = Ok(snapshot.clone());
        assert_eq!(fetch(&b, Domain::Displays), Update::Displays(Ok(snapshot)));
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] };
        assert_eq!(perform(&b, &Op::TryDisplays(plan)), Ok(()));
        assert_eq!(perform(&b, &Op::KeepDisplays), Ok(()));
        assert_eq!(perform(&b, &Op::RevertDisplays), Ok(()));
        assert_eq!(fake.calls(), vec!["snapshot", "try eDP-1", "keep", "revert"]);
        assert!(f.calls().is_empty(), "no command: {:?}", f.calls());
        // The daemon's words become the error, as they are.
        *fake.answer.lock().unwrap() = Err("another change is in progress".into());
        let e = perform(&b, &Op::KeepDisplays).unwrap_err();
        assert_eq!(e.to_string(), "another change is in progress");
    }

    #[test]
    fn display_settings_are_never_coalesced() {
        // A trial then its rollback: both pass, in order.
        let trial = Op::TryDisplays(Plan::default());
        let rollback = Op::RevertDisplays;
        let (r, dropped) = coalesce(vec![Request::Do(trial.clone()), Request::Do(rollback.clone())]);
        assert_eq!(r, vec![Request::Do(trial), Request::Do(rollback)]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn toggles_are_never_coalesced() {
        let (r, dropped) = coalesce(vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::SetWifi(true))]);
        assert_eq!(r.len(), 2);
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_thread_responds() {
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
    fn each_setting_gets_a_response_even_when_coalesced() {
        // The thread is busy for 200 ms on a slow read: the three settings
        // pile up behind it and form a single burst.
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(200)));
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Dnd])).unwrap();
        // The call is recorded before the wait: as soon as it shows up, the
        // thread is busy and its current burst already formed.
        let t0 = Instant::now();
        while f.calls().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "the thread did not start");
            std::thread::yield_now();
        }
        tx.send(Request::Do(Op::SetBrightness(30))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(40))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(50))).unwrap();
        let t = Duration::from_secs(2);
        assert!(matches!(rx.recv_timeout(t).unwrap(), Update::Dnd(_)));
        let mut received = Vec::new();
        while received.len() < 3 {
            received.push(rx.recv_timeout(t).expect("one response per setting"));
        }
        let last = received.pop().unwrap();
        assert!(matches!(last, Update::Done(Op::SetBrightness(50), _)), "{last:?}");
        assert_eq!(received, vec![Update::Superseded(Op::SetBrightness(30)), Update::Superseded(Op::SetBrightness(40))]);
    }

    #[test]
    fn domains_touched_by_an_action() {
        assert_eq!(Op::SetWifi(true).domains(), &[Domain::Network]);
        assert_eq!(Op::ToggleKeepAwake { now_on: true }.domains(), &[Domain::KeepAwake]);
        assert!(Op::SetVolume(NodeRef::DefaultSink, 5).domains().contains(&Domain::Audio));
        assert!(Op::SetDefault(64).domains().contains(&Domain::Volume));
        assert!(Op::Power(crate::backends::PowerAction::Lock).domains().is_empty());
        for op in [Op::TryDisplays(Plan::default()), Op::KeepDisplays, Op::RevertDisplays] {
            assert_eq!(op.domains(), &[Domain::Displays]);
        }
    }

    #[test]
    fn spawn_does_not_block_the_caller() {
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(300)));
        let (tx, rx) = spawn(backends(&f));
        let t = Instant::now();
        tx.send(Request::Refresh(Tab::Home.domains().to_vec())).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50));
        // The first domain arrives without waiting for the other six.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Update::Volume(Err(_))));
    }
}
