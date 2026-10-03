//! État et logique d'alpheus, sans terminal ni thread : chaque entrée
//! (touche, souris, réponse du worker, tic) modifie l'état ; les requêtes au
//! worker et l'outil externe à ouvrir sont déposés dans des boîtes que la
//! boucle réelle (run.rs) vide. Tout se teste en appelant des méthodes.

pub mod action;
pub mod keymap;
pub mod run;
pub mod worker;
mod input;
mod mouse;

use crate::backends::{
    brightness, AudioNode, AudioStatus, BResult, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, Volume,
};
use crate::config::Config;
use crate::fuzzy;
use crate::theme::Theme;
use action::{palette_entries, profile_label, Command, External, Tab};
use keymap::Keymap;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use worker::{domains, Domain, Op, Request, Update};

/// Pas des curseurs (touches -/+, molette).
pub const STEP: i32 = 5;
/// Durée de vie d'un message éphémère.
pub const TOAST_TTL: Duration = Duration::from_secs(3);
/// Un domaine en échec est relu au plus une fois par cette période.
pub const FAILED_RETRY: Duration = Duration::from_secs(15);

/// Période de rafraîchissement de l'onglet visible : l'Accueil chaque
/// seconde, Son/Réseau/Bluetooth toutes les 2 s, l'Alimentation jamais
/// (rien à lire).
pub fn refresh_period(tab: Tab) -> Option<Duration> {
    match tab {
        Tab::Accueil => Some(Duration::from_secs(1)),
        Tab::Son | Tab::Reseau | Tab::Bluetooth => Some(Duration::from_secs(2)),
        Tab::Alimentation => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Loadable<T> {
    #[default]
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Loadable::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Loadable::Failed(_))
    }

    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Loadable<U> {
        match self {
            Loadable::Loading => Loadable::Loading,
            Loadable::Ready(v) => Loadable::Ready(f(v)),
            Loadable::Failed(e) => Loadable::Failed(e.clone()),
        }
    }
}

impl<T> From<BResult<T>> for Loadable<T> {
    fn from(r: BResult<T>) -> Self {
        match r {
            Ok(v) => Loadable::Ready(v),
            Err(e) => Loadable::Failed(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Data {
    pub volume: Loadable<Volume>,
    pub brightness: Loadable<u8>,
    pub network: Loadable<NetStatus>,
    pub bluetooth: Loadable<BtStatus>,
    pub dnd: Loadable<bool>,
    pub keep_awake: Loadable<bool>,
    pub profiles: Loadable<Profiles>,
    pub audio: Loadable<AudioStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliderId {
    Volume,
    Brightness,
    Node(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToggleId {
    Wifi,
    Bluetooth,
    Dnd,
    KeepAwake,
}

/// Élément focalisable de l'onglet courant (ordre = ordre d'affichage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Slider(SliderId),
    Toggle(ToggleId),
    Profile,
    Button(Command),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    pub action: PowerAction,
    /// Le focus démarre sur « Annuler » : Entrée par réflexe n'éteint rien.
    pub yes_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Palette(Palette),
    Confirm(Confirm),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Search {
    pub text: String,
    pub editing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub last: Option<String>,
    /// Dernière erreur d'action ; effacée au succès suivant.
    pub error: Option<String>,
    /// Avertissement de démarrage (config, thème, touches) : persistant.
    pub warning: Option<String>,
}

pub fn toggle_label(id: ToggleId) -> &'static str {
    match id {
        ToggleId::Wifi => "Wi-Fi",
        ToggleId::Bluetooth => "Bluetooth",
        ToggleId::Dnd => "Ne pas déranger",
        ToggleId::KeepAwake => "Garder éveillé",
    }
}

fn op_label(op: &Op) -> String {
    match op {
        Op::SetVolume(..) => "Volume".into(),
        Op::SetDefault(_) => "Périphérique par défaut".into(),
        Op::SetBrightness(_) => "Luminosité".into(),
        Op::SetWifi(_) => "Wi-Fi".into(),
        Op::SetBluetooth(_) => "Bluetooth".into(),
        Op::SetDnd(_) => "Ne pas déranger".into(),
        Op::ToggleKeepAwake { .. } => "Garder éveillé".into(),
        Op::SetProfile(_) => "Profil d'énergie".into(),
        Op::Power(a) => a.label().into(),
    }
}

/// Domaine relu par une mise à jour (aucun pour une réponse d'action).
fn update_domain(u: &Update) -> Option<Domain> {
    Some(match u {
        Update::Volume(_) => Domain::Volume,
        Update::Brightness(_) => Domain::Brightness,
        Update::Network(_) => Domain::Network,
        Update::Bluetooth(_) => Domain::Bluetooth,
        Update::Dnd(_) => Domain::Dnd,
        Update::KeepAwake(_) => Domain::KeepAwake,
        Update::Profiles(_) => Domain::Profiles,
        Update::Audio(_) => Domain::Audio,
        Update::Done(..) | Update::Superseded(_) => return None,
    })
}

pub struct App {
    pub tab: Tab,
    pub data: Data,
    pub focus: usize,
    pub overlay: Overlay,
    pub search: Search,
    pub status: Status,
    pub toasts: Vec<Toast>,
    pub keymap: Keymap,
    pub config: Config,
    pub theme: Theme,
    pub should_quit: bool,
    /// Curseur en cours de glissement à la souris, et sa barre.
    pub(crate) dragging: Option<(SliderId, Rect)>,
    last_refresh: Instant,
    requests: Vec<Request>,
    external: Option<External>,
    /// Actions envoyées et encore sans réponse, par domaine touché. Tant
    /// qu'un domaine en a, ses relectures sont ignorées : lancées avant
    /// l'action, elles ramèneraient l'ancien état par-dessus l'affichage
    /// optimiste (clignotement). Le fil répond à chaque `Request::Do` par
    /// exactement un `Done` ou un `Superseded` (réglage fusionné dans une
    /// rafale), donc le compte revient toujours à zéro.
    pending: HashMap<Domain, u32>,
    /// Dernière demande de lecture, par domaine (relecture lente des pannes).
    last_read: HashMap<Domain, Instant>,
}

impl App {
    pub fn new(config: Config, theme: Theme, keymap: Keymap, now: Instant) -> App {
        let mut app = App {
            tab: Tab::Accueil,
            data: Data::default(),
            focus: 0,
            overlay: Overlay::None,
            search: Search::default(),
            status: Status::default(),
            toasts: Vec::new(),
            keymap,
            config,
            theme,
            should_quit: false,
            dragging: None,
            last_refresh: now,
            requests: Vec::new(),
            external: None,
            pending: HashMap::new(),
            last_read: HashMap::new(),
        };
        app.refresh_tab(now);
        app
    }

    /// Requêtes à transmettre au worker (vide la boîte).
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Outil externe à ouvrir (vide la boîte).
    pub fn take_external(&mut self) -> Option<External> {
        self.external.take()
    }

    /// Avertissement de démarrage (config, thème, touches) : ligne d'état.
    pub fn warn(&mut self, msg: String) {
        self.status.warning = Some(match self.status.warning.take() {
            Some(prev) => format!("{prev} ; {msg}"), // plusieurs fichiers fautifs
            None => msg,
        });
    }

    pub fn external_argv(&self, e: External) -> Vec<String> {
        let c = &self.config.commands;
        match e {
            External::Mixer => c.mixer.clone(),
            External::Network => c.network.clone(),
            External::Bluetooth => c.bluetooth.clone(),
        }
    }

    pub fn external_done(&mut self, e: External, r: Result<(), String>, now: Instant) {
        if let Err(msg) = r {
            let msg = format!("{} : {msg}", e.label());
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
        }
        self.refresh_tab(now); // l'outil a pu tout changer
    }

    // ── Réponses du worker ───────────────────────────────────────────

    /// Envoie une action au fil et note les domaines qu'elle touche.
    fn send_op(&mut self, op: Op) {
        for d in op.domains() {
            *self.pending.entry(*d).or_default() += 1;
        }
        self.requests.push(Request::Do(op));
    }

    /// L'action a reçu sa réponse (faite ou abandonnée par fusion).
    fn op_answered(&mut self, op: &Op) {
        for d in op.domains() {
            if let Some(n) = self.pending.get_mut(d) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.pending.remove(d);
                }
            }
        }
    }

    fn is_pending(&self, d: Domain) -> bool {
        self.pending.contains_key(&d)
    }

    pub fn on_update(&mut self, u: Update, now: Instant) {
        if let Some(d) = update_domain(&u) {
            if self.is_pending(d) {
                return; // relecture périmée : une action sur ce domaine est en cours
            }
        }
        match u {
            Update::Volume(r) => self.data.volume = r.into(),
            Update::Brightness(r) => self.data.brightness = r.into(),
            Update::Network(r) => self.data.network = r.into(),
            Update::Bluetooth(r) => self.data.bluetooth = r.into(),
            Update::Dnd(r) => self.data.dnd = r.into(),
            Update::KeepAwake(r) => self.data.keep_awake = r.into(),
            Update::Profiles(r) => self.data.profiles = r.into(),
            Update::Audio(r) => self.data.audio = r.into(),
            Update::Done(op, r) => {
                self.op_answered(&op);
                self.op_done(op, r, now);
            }
            Update::Superseded(op) => self.op_answered(&op),
        }
        self.clamp_focus();
    }

    fn op_done(&mut self, op: Op, r: BResult<()>, now: Instant) {
        match r {
            Ok(()) => {
                self.status.error = None;
                if let Some(msg) = self.success_message(&op) {
                    self.status.last = Some(msg.clone());
                    self.toast(msg, false, now);
                }
                // Curseurs : la valeur affichée est déjà la bonne ; relire
                // pendant un glissement ferait sautiller le curseur.
                if !matches!(op, Op::SetVolume(..) | Op::SetBrightness(_)) {
                    self.refresh_tab(now);
                }
            }
            Err(e) => {
                let msg = format!("{} : {e}", op_label(&op));
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.refresh_tab(now); // annule l'affichage optimiste
            }
        }
    }

    fn success_message(&self, op: &Op) -> Option<String> {
        let onoff = |b: bool| if b { "activé" } else { "désactivé" };
        Some(match op {
            Op::SetVolume(..) | Op::SetBrightness(_) => return None,
            Op::SetDefault(id) => {
                let a = self.data.audio.ready()?;
                match a.sinks.iter().find(|n| n.id == *id) {
                    Some(n) => format!("Sortie : {}", n.name),
                    None => format!("Entrée : {}", a.sources.iter().find(|n| n.id == *id)?.name),
                }
            }
            Op::SetWifi(b) => format!("Wi-Fi : {}", onoff(*b)),
            Op::SetBluetooth(b) => format!("Bluetooth : {}", onoff(*b)),
            Op::SetDnd(b) => format!("Ne pas déranger : {}", onoff(*b)),
            Op::ToggleKeepAwake { now_on } => format!("Garder éveillé : {}", onoff(*now_on)),
            Op::SetProfile(p) => format!("Profil : {}", profile_label(p)),
            Op::Power(a) => a.label().to_string(),
        })
    }

    fn toast(&mut self, text: String, error: bool, now: Instant) {
        self.toasts.push(Toast { text, error, until: now + TOAST_TTL });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    // ── Temps ────────────────────────────────────────────────────────

    pub fn on_tick(&mut self, now: Instant) {
        self.toasts.retain(|t| t.until > now);
        let Some(period) = refresh_period(self.tab) else { return };
        let due = now.duration_since(self.last_refresh) >= period;
        // Un domaine en échec n'est pas sondé au tic normal : un outil cassé
        // (shell-ctl notifie chacun de ses échecs) ne doit pas être relancé
        // chaque seconde. Il est relu au plus toutes les 15 s, et à l'entrée
        // dans l'onglet ou après une action.
        let ds: Vec<Domain> = domains(self.tab)
            .iter()
            .copied()
            .filter(|d| {
                if self.domain_failed(*d) {
                    self.last_read.get(d).is_none_or(|t| now.duration_since(*t) >= FAILED_RETRY)
                } else {
                    due
                }
            })
            .collect();
        if due {
            self.last_refresh = now;
        }
        self.push_refresh(ds, now);
    }

    /// Demande la relecture de `ds` et note l'heure, domaine par domaine.
    fn push_refresh(&mut self, ds: Vec<Domain>, now: Instant) {
        if ds.is_empty() {
            return;
        }
        for d in &ds {
            self.last_read.insert(*d, now);
        }
        self.requests.push(Request::Refresh(ds));
    }

    fn domain_failed(&self, d: Domain) -> bool {
        match d {
            Domain::Volume => self.data.volume.is_failed(),
            Domain::Brightness => self.data.brightness.is_failed(),
            Domain::Dnd => self.data.dnd.is_failed(),
            Domain::KeepAwake => self.data.keep_awake.is_failed(),
            Domain::Profiles => self.data.profiles.is_failed(),
            Domain::Network => self.data.network.is_failed(),
            Domain::Bluetooth => self.data.bluetooth.is_failed(),
            Domain::Audio => self.data.audio.is_failed(),
        }
    }

    fn refresh_tab(&mut self, now: Instant) {
        self.push_refresh(domains(self.tab).to_vec(), now);
        self.last_refresh = now;
    }

    // ── Navigation ───────────────────────────────────────────────────

    pub fn go_tab(&mut self, t: Tab, now: Instant) {
        if t != self.tab {
            self.tab = t;
            self.focus = 0;
            self.search = Search::default();
        }
        self.refresh_tab(now);
    }

    pub fn items(&self) -> Vec<Item> {
        match self.tab {
            Tab::Accueil => vec![
                Item::Slider(SliderId::Volume),
                Item::Slider(SliderId::Brightness),
                Item::Toggle(ToggleId::Wifi),
                Item::Toggle(ToggleId::Bluetooth),
                Item::Toggle(ToggleId::Dnd),
                Item::Toggle(ToggleId::KeepAwake),
                Item::Profile,
            ],
            Tab::Son => {
                let (sinks, sources) = self.visible_nodes();
                let mut v: Vec<Item> =
                    sinks.iter().chain(sources.iter()).map(|n| Item::Slider(SliderId::Node(n.id))).collect();
                v.push(Item::Button(Command::Open(External::Mixer)));
                v
            }
            Tab::Reseau => vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))],
            Tab::Bluetooth => {
                vec![Item::Toggle(ToggleId::Bluetooth), Item::Button(Command::Open(External::Bluetooth))]
            }
            Tab::Alimentation => PowerAction::ALL.iter().map(|a| Item::Button(Command::Power(*a))).collect(),
        }
    }

    pub fn focused(&self) -> Option<Item> {
        self.items().get(self.focus).cloned()
    }

    pub(crate) fn move_focus(&mut self, delta: i32) {
        let n = self.items().len() as i32;
        self.focus = if n == 0 { 0 } else { (self.focus as i32 + delta).clamp(0, n - 1) as usize };
    }

    pub(crate) fn clamp_focus(&mut self) {
        let n = self.items().len();
        if self.focus >= n {
            self.focus = n.saturating_sub(1);
        }
    }

    /// Sorties puis entrées, filtrées par la recherche.
    pub fn visible_nodes(&self) -> (Vec<&AudioNode>, Vec<&AudioNode>) {
        match self.data.audio.ready() {
            None => (Vec::new(), Vec::new()),
            Some(a) => (
                a.sinks.iter().filter(|n| self.matches_search(&n.name)).collect(),
                a.sources.iter().filter(|n| self.matches_search(&n.name)).collect(),
            ),
        }
    }

    pub fn matches_search(&self, label: &str) -> bool {
        self.search.text.is_empty() || fuzzy::score(&self.search.text, label).is_some()
    }

    // ── Commandes ────────────────────────────────────────────────────

    pub fn run_command(&mut self, c: Command, now: Instant) {
        match c {
            Command::ToggleWifi => self.toggle(ToggleId::Wifi, now),
            Command::ToggleBluetooth => self.toggle(ToggleId::Bluetooth, now),
            Command::ToggleDnd => self.toggle(ToggleId::Dnd, now),
            Command::ToggleKeepAwake => self.toggle(ToggleId::KeepAwake, now),
            Command::SetProfile(name) => {
                if let Loadable::Ready(p) = &mut self.data.profiles {
                    p.active = name.clone();
                }
                self.send_op(Op::SetProfile(name));
            }
            Command::Power(a) if a.needs_confirm() => {
                self.overlay = Overlay::Confirm(Confirm { action: a, yes_focused: false });
            }
            Command::Power(a) => self.send_op(Op::Power(a)),
            Command::Open(e) => self.external = Some(e),
            Command::GoTab(t) => self.go_tab(t, now),
        }
    }

    pub(crate) fn toggle(&mut self, id: ToggleId, now: Instant) {
        let Some(on) = self.toggle_loadable(id).ready().copied() else {
            self.toast(format!("{} : état inconnu", toggle_label(id)), true, now);
            return;
        };
        let op = match id {
            ToggleId::Wifi => {
                if let Loadable::Ready(n) = &mut self.data.network {
                    n.wifi_enabled = !on;
                }
                Op::SetWifi(!on)
            }
            ToggleId::Bluetooth => {
                if let Loadable::Ready(b) = &mut self.data.bluetooth {
                    b.powered = !on;
                }
                Op::SetBluetooth(!on)
            }
            ToggleId::Dnd => {
                self.data.dnd = Loadable::Ready(!on);
                Op::SetDnd(!on)
            }
            ToggleId::KeepAwake => {
                self.data.keep_awake = Loadable::Ready(!on);
                Op::ToggleKeepAwake { now_on: !on }
            }
        };
        self.send_op(op);
    }

    /// Entrée (ou clic) sur l'élément focalisé.
    pub(crate) fn activate(&mut self, now: Instant) {
        match self.focused() {
            Some(Item::Toggle(id)) => self.toggle(id, now),
            Some(Item::Button(c)) => self.run_command(c, now),
            Some(Item::Slider(SliderId::Node(id))) => self.set_default(id),
            _ => {}
        }
    }

    /// -/+ ou flèches gauche/droite sur l'élément focalisé.
    pub(crate) fn adjust(&mut self, dir: i32, now: Instant) {
        match self.focused() {
            Some(Item::Slider(id)) => self.nudge_slider(id, dir * STEP),
            Some(Item::Profile) => self.cycle_profile(dir, now),
            _ => {}
        }
    }

    pub fn set_slider(&mut self, id: SliderId, value: i32) {
        let min = if id == SliderId::Brightness { i32::from(brightness::MIN_PERCENT) } else { 0 };
        let v = value.clamp(min, 100) as u8;
        match self.slider_value(id) {
            None => return, // état inconnu : on n'envoie rien à l'aveugle
            Some(cur) if cur == v => return,
            // Volume poussé au-delà de 100 % (wpctl le permet) : une hausse
            // demandée serait bornée à 100 et baisserait le son. Rien n'est
            // envoyé ; une baisse, elle, descend normalement (à 100 au plus).
            Some(cur) if cur > 100 && value > i32::from(cur) => return,
            Some(_) => {}
        }
        let op = match id {
            SliderId::Volume => {
                if let Loadable::Ready(vol) = &mut self.data.volume {
                    vol.percent = v;
                }
                Op::SetVolume(NodeRef::DefaultSink, v)
            }
            SliderId::Brightness => {
                self.data.brightness = Loadable::Ready(v);
                Op::SetBrightness(v)
            }
            SliderId::Node(n) => {
                if let Loadable::Ready(a) = &mut self.data.audio {
                    for node in a.sinks.iter_mut().chain(a.sources.iter_mut()) {
                        if node.id == n {
                            node.volume.percent = v;
                        }
                    }
                }
                Op::SetVolume(NodeRef::Id(n), v)
            }
        };
        self.send_op(op);
    }

    pub fn nudge_slider(&mut self, id: SliderId, delta: i32) {
        if let Some(cur) = self.slider_value(id) {
            self.set_slider(id, i32::from(cur) + delta);
        }
    }

    fn set_default(&mut self, id: u32) {
        if let Loadable::Ready(a) = &mut self.data.audio {
            for list in [&mut a.sinks, &mut a.sources] {
                if list.iter().any(|n| n.id == id) {
                    for n in list.iter_mut() {
                        n.default = n.id == id;
                    }
                }
            }
        }
        self.send_op(Op::SetDefault(id));
    }

    fn cycle_profile(&mut self, dir: i32, now: Instant) {
        let Some(p) = self.data.profiles.ready() else { return };
        let Some(cur) = p.available.iter().position(|n| *n == p.active) else { return };
        let next = (cur as i32 + dir).clamp(0, p.available.len() as i32 - 1) as usize;
        if next != cur {
            let name = p.available[next].clone();
            self.run_command(Command::SetProfile(name), now);
        }
    }

    pub(crate) fn close_confirm(&mut self, yes: bool) {
        if let Overlay::Confirm(c) = self.overlay {
            self.overlay = Overlay::None;
            if yes {
                self.send_op(Op::Power(c.action));
            }
        }
    }

    pub fn palette_matches(&self) -> Vec<(String, Command)> {
        let query = match &self.overlay {
            Overlay::Palette(p) => p.query.as_str(),
            _ => "",
        };
        let profiles = self.data.profiles.ready().map(|p| p.available.clone()).unwrap_or_default();
        let entries = palette_entries(&profiles);
        let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_str()).collect();
        fuzzy::rank(query, &labels).into_iter().map(|i| entries[i].clone()).collect()
    }

    /// Lance l'entrée `index` (ou la sélection) de la palette, puis la ferme.
    pub(crate) fn palette_run(&mut self, index: Option<usize>, now: Instant) {
        let sel = match &self.overlay {
            Overlay::Palette(p) => index.unwrap_or(p.selected),
            _ => return,
        };
        let matches = self.palette_matches();
        self.overlay = Overlay::None;
        if let Some((_, c)) = matches.into_iter().nth(sel) {
            self.run_command(c, now);
        }
    }

    // ── Vues pour l'affichage ────────────────────────────────────────

    fn node(&self, id: u32) -> Option<&AudioNode> {
        self.data.audio.ready()?.sinks.iter().chain(&self.data.audio.ready()?.sources).find(|n| n.id == id)
    }

    pub fn slider_loadable(&self, id: SliderId) -> Loadable<u8> {
        match id {
            SliderId::Volume => self.data.volume.map(|v| v.percent),
            SliderId::Brightness => self.data.brightness.clone(),
            SliderId::Node(n) => match &self.data.audio {
                Loadable::Ready(_) => self.node(n).map_or(Loadable::Loading, |x| Loadable::Ready(x.volume.percent)),
                other => other.map(|_| 0),
            },
        }
    }

    pub fn slider_value(&self, id: SliderId) -> Option<u8> {
        self.slider_loadable(id).ready().copied()
    }

    pub fn slider_muted(&self, id: SliderId) -> bool {
        match id {
            SliderId::Volume => self.data.volume.ready().is_some_and(|v| v.muted),
            SliderId::Brightness => false,
            SliderId::Node(n) => self.node(n).is_some_and(|x| x.volume.muted),
        }
    }

    pub fn slider_label(&self, id: SliderId) -> String {
        match id {
            SliderId::Volume => "Volume".into(),
            SliderId::Brightness => "Luminosité".into(),
            SliderId::Node(n) => match self.node(n) {
                Some(x) => format!("{} {}", if x.default { "●" } else { " " }, x.name),
                None => "?".into(),
            },
        }
    }

    pub fn toggle_loadable(&self, id: ToggleId) -> Loadable<bool> {
        match id {
            ToggleId::Wifi => self.data.network.map(|n| n.wifi_enabled),
            ToggleId::Bluetooth => self.data.bluetooth.map(|b| b.powered),
            ToggleId::Dnd => self.data.dnd.clone(),
            ToggleId::KeepAwake => self.data.keep_awake.clone(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::backends::{BtDevice, Connection};

    pub fn app() -> App {
        App::new(Config::default(), Theme::default(), Keymap::default(), Instant::now())
    }

    pub fn node(id: u32, name: &str, percent: u8, default: bool) -> AudioNode {
        AudioNode { id, name: name.into(), volume: Volume { percent, muted: false }, default }
    }

    /// Données prêtes, reprises des relevés du poste.
    pub fn ready_app() -> App {
        let mut a = app();
        a.take_requests();
        a.data = Data {
            volume: Loadable::Ready(Volume { percent: 62, muted: false }),
            brightness: Loadable::Ready(20),
            network: Loadable::Ready(NetStatus {
                wifi_enabled: true,
                active: vec![
                    Connection { name: "MON_WIFI_1".into(), kind: "Wi-Fi".into(), device: "wlp0s1f01".into() },
                    Connection { name: "Wired connection 1".into(), kind: "Ethernet".into(), device: "enp0s0u1u2i10".into() },
                ],
            }),
            bluetooth: Loadable::Ready(BtStatus {
                powered: true,
                connected: vec![BtDevice { mac: "AA:BB:CC:DD:EE:01".into(), name: "CasqueBT".into() }],
            }),
            dnd: Loadable::Ready(false),
            keep_awake: Loadable::Ready(false),
            profiles: Loadable::Ready(Profiles {
                active: "balanced".into(),
                available: vec!["performance".into(), "balanced".into(), "power-saver".into()],
            }),
            audio: Loadable::Ready(AudioStatus {
                sinks: vec![node(64, "Dock D6000 Dell", 75, false), node(116, "CasqueBT", 62, true)],
                sources: vec![node(113, "Stereo Microphone", 100, true)],
            }),
        };
        a
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::backends::BackendError;

    fn later(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn demarrage_demande_l_accueil() {
        let mut a = app();
        assert_eq!(a.tab, Tab::Accueil);
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Accueil).to_vec())]);
        assert_eq!(a.data.volume, Loadable::Loading);
    }

    #[test]
    fn mises_a_jour() {
        let mut a = app();
        a.on_update(Update::Volume(Ok(Volume { percent: 40, muted: true })), Instant::now());
        a.on_update(Update::Brightness(Err(BackendError::Missing { tool: "brightnessctl".into() })), Instant::now());
        assert_eq!(a.data.volume, Loadable::Ready(Volume { percent: 40, muted: true }));
        assert_eq!(a.data.brightness, Loadable::Failed("brightnessctl introuvable".into()));
    }

    #[test]
    fn curseur_optimiste_et_requete() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Volume, STEP);
        assert_eq!(a.slider_value(SliderId::Volume), Some(67));
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67))]);
        a.nudge_slider(SliderId::Node(64), -STEP);
        assert_eq!(a.slider_value(SliderId::Node(64)), Some(70));
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 70))]);
    }

    #[test]
    fn curseurs_bornes() {
        let mut a = ready_app();
        a.set_slider(SliderId::Volume, 150);
        a.set_slider(SliderId::Brightness, -20);
        assert_eq!(a.take_requests(), vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100)),
            Request::Do(Op::SetBrightness(crate::backends::brightness::MIN_PERCENT)),
        ]);
    }

    #[test]
    fn volume_au_dessus_de_100_jamais_baisse_par_plus() {
        let mut a = ready_app();
        a.data.volume = Loadable::Ready(Volume { percent: 120, muted: false });
        a.nudge_slider(SliderId::Volume, STEP);
        assert!(a.take_requests().is_empty(), "« + » ne ramène pas 120 % à 100 %");
        assert_eq!(a.slider_value(SliderId::Volume), Some(120));
        a.nudge_slider(SliderId::Volume, -STEP);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100))], "« - » borne à 100");
        assert_eq!(a.slider_value(SliderId::Volume), Some(100));
        // Même règle pour un périphérique de l'onglet Son.
        if let Loadable::Ready(st) = &mut a.data.audio {
            st.sinks[0].volume.percent = 130;
        }
        a.nudge_slider(SliderId::Node(64), STEP);
        assert!(a.take_requests().is_empty());
        a.nudge_slider(SliderId::Node(64), -STEP);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 100))]);
    }

    #[test]
    fn curseur_inconnu_ou_inchange_n_envoie_rien() {
        let mut a = app();
        a.take_requests();
        a.nudge_slider(SliderId::Volume, STEP);
        assert!(a.take_requests().is_empty());
        let mut a = ready_app();
        a.set_slider(SliderId::Volume, 62);
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn bascule_optimiste() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false));
        a.toggle(ToggleId::KeepAwake, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::ToggleKeepAwake { now_on: true })]);
    }

    fn net(wifi: bool) -> Update {
        Update::Network(Ok(NetStatus { wifi_enabled: wifi, active: Vec::new() }))
    }

    #[test]
    fn relecture_perimee_ignoree_pendant_une_bascule() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        a.take_requests();
        // Relecture lancée avant la bascule : elle porte l'ancien état.
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "l'affichage reste optimiste");
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        // Après la réponse, la relecture suivante fait foi.
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
    }

    #[test]
    fn les_autres_domaines_restent_relus_pendant_une_bascule() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        a.on_update(Update::Dnd(Ok(true)), Instant::now());
        assert_eq!(a.data.dnd, Loadable::Ready(true));
    }

    #[test]
    fn deux_bascules_attendent_les_deux_reponses() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now()); // → désactivé
        a.toggle(ToggleId::Wifi, Instant::now()); // → activé
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        a.on_update(net(false), Instant::now()); // lue entre les deux actions
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
        a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
        a.on_update(net(false), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "plus rien en attente");
    }

    #[test]
    fn rafale_de_curseur_fusionnee_ne_bloque_pas_le_domaine() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Brightness, STEP); // 25
        a.nudge_slider(SliderId::Brightness, STEP); // 30
        a.nudge_slider(SliderId::Brightness, STEP); // 35
        a.on_update(Update::Brightness(Ok(20)), Instant::now());
        assert_eq!(a.data.brightness, Loadable::Ready(35), "relecture périmée ignorée");
        // Le fil a fusionné la rafale : deux réglages abandonnés, un seul fait.
        a.on_update(Update::Superseded(Op::SetBrightness(25)), Instant::now());
        a.on_update(Update::Superseded(Op::SetBrightness(30)), Instant::now());
        a.on_update(Update::Done(Op::SetBrightness(35), Ok(())), Instant::now());
        a.on_update(Update::Brightness(Ok(40)), Instant::now());
        assert_eq!(a.data.brightness, Loadable::Ready(40), "le domaine n'est pas resté bloqué");
    }

    #[test]
    fn volume_par_defaut_bloque_aussi_la_liste_audio() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Volume, STEP);
        a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
        assert_eq!(a.data.audio.ready().unwrap().sinks.len(), 2);
        a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 67), Ok(())), Instant::now());
        a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
        assert!(a.data.audio.ready().unwrap().sinks.is_empty());
    }

    #[test]
    fn message_garder_eveille_malgre_une_valeur_perimee() {
        let mut a = ready_app(); // désactivé
        a.toggle(ToggleId::KeepAwake, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::ToggleKeepAwake { now_on: true })]);
        a.on_update(Update::KeepAwake(Ok(false)), Instant::now()); // lue avant la bascule
        a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
        assert_eq!(a.toasts[0].text, "Garder éveillé : activé");
        // Même si l'état affiché était périmé, le message suit l'état visé.
        a.data.keep_awake = Loadable::Ready(false);
        a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
        assert_eq!(a.toasts[1].text, "Garder éveillé : activé");
    }

    #[test]
    fn bascule_d_un_etat_inconnu() {
        let mut a = app();
        a.take_requests();
        a.toggle(ToggleId::Dnd, Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.toasts[0].text, "Ne pas déranger : état inconnu");
        assert!(a.toasts[0].error);
    }

    #[test]
    fn changer_d_onglet() {
        let mut a = ready_app();
        a.focus = 3;
        a.search.text = "x".into();
        a.go_tab(Tab::Son, Instant::now());
        assert_eq!((a.tab, a.focus), (Tab::Son, 0));
        assert_eq!(a.search, Search::default());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
        a.go_tab(Tab::Alimentation, Instant::now());
        assert!(a.take_requests().is_empty()); // rien à lire
    }

    #[test]
    fn elements_par_onglet() {
        let mut a = ready_app();
        assert_eq!(a.items().len(), 7);
        a.go_tab(Tab::Son, Instant::now());
        assert_eq!(a.items(), vec![
            Item::Slider(SliderId::Node(64)),
            Item::Slider(SliderId::Node(116)),
            Item::Slider(SliderId::Node(113)),
            Item::Button(Command::Open(External::Mixer)),
        ]);
        a.go_tab(Tab::Alimentation, Instant::now());
        assert_eq!(a.items().len(), 5);
        a.go_tab(Tab::Reseau, Instant::now());
        assert_eq!(a.items(), vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))]);
    }

    #[test]
    fn filtre_de_recherche() {
        let mut a = ready_app();
        a.go_tab(Tab::Son, Instant::now());
        a.search.text = "casq".into();
        let (sinks, sources) = a.visible_nodes();
        assert_eq!(sinks.iter().map(|n| n.id).collect::<Vec<_>>(), vec![116]);
        assert!(sources.is_empty());
        assert_eq!(a.items().len(), 2);
        a.focus = 3;
        a.clamp_focus();
        assert_eq!(a.focus, 1);
    }

    #[test]
    fn activer_une_sortie_la_rend_par_defaut() {
        let mut a = ready_app();
        a.go_tab(Tab::Son, Instant::now());
        a.take_requests();
        a.activate(Instant::now());
        let st = a.data.audio.ready().unwrap();
        assert!(st.sinks[0].default && !st.sinks[1].default);
        assert!(st.sources[0].default, "les entrées ne bougent pas");
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
    }

    #[test]
    fn profil_borne() {
        let mut a = ready_app();
        a.focus = 6;
        a.adjust(1, Instant::now());
        assert_eq!(a.data.profiles.ready().unwrap().active, "power-saver");
        a.adjust(1, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetProfile("power-saver".into()))]);
    }

    #[test]
    fn alimentation_avec_ou_sans_confirmation() {
        let mut a = ready_app();
        a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
        assert_eq!(a.overlay, Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false }));
        assert!(a.take_requests().is_empty());
        a.close_confirm(false);
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.take_requests().is_empty());
        a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
        a.close_confirm(true);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Poweroff))]);
        a.run_command(Command::Power(PowerAction::Lock), Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Lock))]);
    }

    #[test]
    fn outil_externe() {
        let mut a = ready_app();
        a.run_command(Command::Open(External::Mixer), Instant::now());
        assert_eq!(a.take_external(), Some(External::Mixer));
        assert_eq!(a.take_external(), None);
        assert_eq!(a.external_argv(External::Mixer), vec!["wiremix"]);
        a.external_done(External::Mixer, Err("wiremix introuvable".into()), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Mixage par appli : wiremix introuvable"));
        assert!(a.toasts[0].error);
    }

    #[test]
    fn action_reussie() {
        let mut a = ready_app();
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        assert_eq!(a.toasts[0].text, "Wi-Fi : désactivé");
        assert_eq!(a.status.last.as_deref(), Some("Wi-Fi : désactivé"));
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Accueil).to_vec())]);
        a.on_update(Update::Done(Op::SetDefault(116), Ok(())), Instant::now());
        assert_eq!(a.toasts[1].text, "Sortie : CasqueBT");
        a.on_update(Update::Done(Op::SetDefault(113), Ok(())), Instant::now());
        assert_eq!(a.toasts[2].text, "Entrée : Stereo Microphone");
    }

    #[test]
    fn curseur_reussi_sans_message_ni_relecture() {
        let mut a = ready_app();
        a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 50), Ok(())), Instant::now());
        assert!(a.toasts.is_empty());
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn action_echouee() {
        let mut a = ready_app();
        let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boum".into() };
        a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Ne pas déranger : shell-ctl a échoué (code 1) : boum"));
        assert!(a.toasts[0].error);
        assert_eq!(a.take_requests().len(), 1, "relecture pour annuler l'affichage optimiste");
    }

    #[test]
    fn tic_rafraichit_l_accueil_chaque_seconde() {
        let mut a = ready_app();
        a.on_tick(later(1100));
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Accueil).to_vec())]);
        a.on_tick(later(1500));
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn tic_ne_resonde_pas_un_domaine_en_echec() {
        let mut a = ready_app();
        a.data.dnd = Loadable::Failed("shell-ctl a échoué (code 1)".into());
        a.on_tick(later(1100));
        let r = a.take_requests();
        assert_eq!(r.len(), 1);
        let Request::Refresh(ds) = &r[0] else { panic!("{r:?}") };
        assert!(!ds.contains(&Domain::Dnd));
        assert_eq!(ds.len(), 6);
    }

    #[test]
    fn tic_rafraichit_son_reseau_bluetooth_toutes_les_deux_secondes() {
        for (tab, d) in [(Tab::Son, Domain::Audio), (Tab::Reseau, Domain::Network), (Tab::Bluetooth, Domain::Bluetooth)] {
            let mut a = ready_app();
            let t0 = Instant::now();
            a.go_tab(tab, t0);
            a.take_requests();
            a.on_tick(t0 + Duration::from_millis(1100));
            assert!(a.take_requests().is_empty(), "{tab:?} : pas relu à 1 s");
            a.on_tick(t0 + Duration::from_millis(2100));
            assert_eq!(a.take_requests(), vec![Request::Refresh(vec![d])], "{tab:?} : relu à 2 s");
            a.on_tick(t0 + Duration::from_millis(3000));
            assert!(a.take_requests().is_empty(), "{tab:?} : période repartie de la relecture");
        }
    }

    fn refreshed(r: &[Request]) -> Vec<Domain> {
        r.iter().flat_map(|q| match q {
            Request::Refresh(ds) => ds.clone(),
            Request::Do(_) => Vec::new(),
        }).collect()
    }

    #[test]
    fn domaine_en_echec_relu_toutes_les_quinze_secondes() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Accueil, t0); // lecture complète à t0
        a.take_requests();
        a.data.dnd = Loadable::Failed("shell-ctl a échoué (code 1)".into());
        let at = |ms| t0 + Duration::from_millis(ms);
        a.on_tick(at(1100));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "pas relu à 1 s");
        a.on_tick(at(14_900));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "pas relu avant 15 s");
        a.on_tick(at(15_100));
        let r = refreshed(&a.take_requests());
        assert_eq!(r, vec![Domain::Dnd], "relu à 15 s, sans attendre le tic normal");
        a.on_tick(at(16_200));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "la période de 15 s repart");
        a.on_tick(at(30_200));
        assert!(refreshed(&a.take_requests()).contains(&Domain::Dnd));
    }

    #[test]
    fn onglet_en_echec_relu_a_quinze_secondes_seulement() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Son, t0);
        a.take_requests();
        a.data.audio = Loadable::Failed("wpctl introuvable".into());
        a.on_tick(t0 + Duration::from_millis(2100));
        assert!(a.take_requests().is_empty(), "pas relu au tic de 2 s");
        a.on_tick(t0 + Duration::from_millis(15_100));
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
    }

    #[test]
    fn tic_ne_relit_rien_sur_l_alimentation() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Alimentation, t0);
        a.take_requests();
        a.on_tick(t0 + Duration::from_secs(60));
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn les_messages_expirent_et_sont_limites_a_trois() {
        let mut a = ready_app();
        for _ in 0..5 {
            a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
        }
        assert_eq!(a.toasts.len(), 3);
        a.on_tick(Instant::now() + TOAST_TTL + Duration::from_millis(100));
        assert!(a.toasts.is_empty());
    }

    #[test]
    fn palette_sans_accents() {
        let mut a = ready_app();
        a.overlay = Overlay::Palette(Palette { query: "eteindre".into(), selected: 0 });
        assert_eq!(a.palette_matches()[0].0, "Éteindre");
        a.palette_run(None, Instant::now());
        assert!(matches!(a.overlay, Overlay::Confirm(_)));
    }

    #[test]
    fn avertissement_de_demarrage() {
        let mut a = app();
        a.warn("config.toml invalide : x".into());
        assert_eq!(a.status.warning.as_deref(), Some("config.toml invalide : x"));
        assert_eq!(a.status.error, None);
        a.warn("colors.toml invalide : y".into());
        assert_eq!(a.status.warning.as_deref(), Some("config.toml invalide : x ; colors.toml invalide : y"), "aucun n'est perdu");
    }

    #[test]
    fn l_avertissement_survit_aux_actions() {
        let mut a = ready_app();
        a.warn("config.toml invalide : x".into());
        let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boum".into() };
        a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
        assert!(a.status.error.is_some());
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        assert_eq!(a.status.error, None, "la dernière erreur d'action s'efface au succès suivant");
        assert_eq!(a.status.warning.as_deref(), Some("config.toml invalide : x"), "l'avertissement reste");
    }
}
