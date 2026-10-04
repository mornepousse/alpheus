//! Outils des tests d'intégration du démon : un sway sans tête (compositeur
//! Wayland jetable aux sorties virtuelles), un faux Hyprland, le démon
//! lancé avec son journal. Rien ne touche la session de Mae : dossier
//! d'exécution temporaire, environnement vidé, ni WAYLAND_DISPLAY ni
//! signature de la session.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

pub struct Sway {
    pub runtime: PathBuf,
    pub wayland: String,
    ipc: PathBuf,
    enfant: Child,
}

impl Sway {
    pub fn lancer(nom: &str, sorties: u32) -> Sway {
        let runtime = env::temp_dir().join(format!("alpheus-it-{}-{nom}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let conf = runtime.join("sway.conf");
        fs::write(&conf, "").unwrap();
        let enfant = Command::new("sway")
            .arg("-c")
            .arg(&conf)
            .env_clear()
            .env("PATH", env::var_os("PATH").unwrap_or_default())
            .env("HOME", &runtime)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_HEADLESS_OUTPUTS", sorties.to_string())
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_RENDERER", "pixman")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Son propre groupe de processus (pgid = son propre pid) : sans
            // bus D-Bus de session (XDG_RUNTIME_DIR est ce dossier
            // temporaire, vide), le paquet nixpkgs de sway relaie en fait
            // via `dbus-run-session sway-unwrapped …`. Un SIGKILL sur le
            // seul PID suivi par `Child` tue `dbus-run-session` sans lui
            // laisser le temps de relayer le signal : `dbus-daemon` et le
            // compositeur restent alors orphelins (reparentés à PID 1),
            // vivants. `Drop` ci-dessous tue tout le groupe, pas seulement
            // ce PID.
            .process_group(0)
            .spawn()
            .expect("sway introuvable : lancer ces tests dans `nix develop`");
        // Rangé tout de suite dans `Sway` : son Drop tue et attend sway même
        // si l'attente ci-dessous panique (aucun sway zombie).
        let mut sway = Sway { runtime: runtime.clone(), wayland: String::new(), ipc: PathBuf::new(), enfant };
        let debut = Instant::now();
        loop {
            let noms: Vec<String> = fs::read_dir(&runtime).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            let wayland = noms.iter().find(|n| n.starts_with("wayland-") && !n.ends_with(".lock")).cloned();
            let ipc = noms.iter().find(|n| n.starts_with("sway-ipc.")).map(|n| runtime.join(n));
            if let (Some(wayland), Some(ipc)) = (wayland, ipc) {
                sway.wayland = wayland;
                sway.ipc = ipc;
                return sway;
            }
            assert!(debut.elapsed() < Duration::from_secs(10), "sway sans tête ne démarre pas");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Commande au sway SANS TÊTE de ce test (son propre socket IPC).
    pub fn commande(&self, cmd: &str) {
        let ok = Command::new("swaymsg").arg("-s").arg(&self.ipc).arg(cmd).stdout(Stdio::null()).status().unwrap().success();
        assert!(ok, "swaymsg {cmd}");
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        // Groupe entier (voir `process_group(0)` ci-dessus), pas seulement
        // ce PID : `dbus-run-session`, `dbus-daemon` et le compositeur qu'il
        // a mis en route partagent ce pgid, jamais celui de la session de
        // test. SAFETY : kill(2) vers un groupe que NOUS avons créé à ce
        // lancement (process_group(0)), jamais -1 ni 0.
        let pgid = self.enfant.id() as i32;
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        let _ = self.enfant.wait();
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

const MONITEURS: &str = include_str!("../fixtures/hypr-monitors.json");
const BUREAUX: &str = include_str!("../fixtures/hypr-workspaces.json");
const CLIENTS: &str = include_str!("../fixtures/hypr-clients.json");
const ACTIVE: &str = include_str!("../fixtures/hypr-activewindow.json");

/// Faux Hyprland dans `<runtime>/hypr/<signature>` : état des fixtures,
/// flux d'événements alimenté par le test.
pub struct FauxHyprland {
    flux: Arc<Mutex<Option<UnixStream>>>,
}

impl FauxHyprland {
    pub fn demarrer(runtime: &Path, signature: &str) -> FauxHyprland {
        let dossier = runtime.join("hypr").join(signature);
        fs::create_dir_all(&dossier).unwrap();
        let l1 = UnixListener::bind(dossier.join(".socket.sock")).unwrap();
        thread::spawn(move || {
            for c in l1.incoming() {
                let Ok(mut c) = c else { return };
                let mut buf = [0u8; 256];
                let n = c.read(&mut buf).unwrap_or(0);
                let rep = match &buf[..n] {
                    b"j/monitors" => MONITEURS,
                    b"j/workspaces" => BUREAUX,
                    b"j/clients" => CLIENTS,
                    b"j/activewindow" => ACTIVE,
                    _ => "unknown request",
                };
                let _ = c.write_all(rep.as_bytes());
            }
        });
        let flux = Arc::new(Mutex::new(None));
        let f = flux.clone();
        let l2 = UnixListener::bind(dossier.join(".socket2.sock")).unwrap();
        thread::spawn(move || {
            for c in l2.incoming().flatten() {
                *f.lock().unwrap() = Some(c); // une reconnexion remplace l'ancienne
            }
        });
        FauxHyprland { flux }
    }

    /// Écrit sur le flux s'il est connecté ; rend false sinon.
    pub fn ecrire(&self, lignes: &str) -> bool {
        match self.flux.lock().unwrap().as_mut() {
            Some(c) => c.write_all(lignes.as_bytes()).is_ok(),
            None => false,
        }
    }
}

pub struct Demon {
    pub enfant: Child,
    journal: Arc<Mutex<Vec<String>>>,
}

pub const SIGNATURE: &str = "faux";

pub fn lancer_demon(sway: &Sway) -> Demon {
    let mut enfant = Command::new(env!("CARGO_BIN_EXE_alpheus-demon"))
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("HOME", &sway.runtime)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("XDG_CONFIG_HOME", sway.runtime.join("config"))
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
        .env("ALPHEUS_POLICES", env::var_os("ALPHEUS_POLICES").unwrap_or_default())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let journal = Arc::new(Mutex::new(Vec::new()));
    let j = journal.clone();
    let err = enfant.stderr.take().unwrap();
    thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            j.lock().unwrap().push(l);
        }
    });
    Demon { enfant, journal }
}

impl Demon {
    pub fn pid(&self) -> u32 {
        self.enfant.id()
    }

    /// Attend une ligne du journal contenant `motif`.
    pub fn attendre(&self, motif: &str, delai: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < delai {
            if self.journal.lock().unwrap().iter().any(|l| l.contains(motif)) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub fn journal(&self) -> Vec<String> {
        self.journal.lock().unwrap().clone()
    }

    /// PSS en Ko (/proc/<pid>/smaps_rollup).
    pub fn pss_ko(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Pss:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// Nombre de fils du démon.
    pub fn fils(&self) -> usize {
        fs::read_dir(format!("/proc/{}/task", self.pid())).map(|d| d.count()).unwrap_or(0)
    }

    /// Changements de contexte cumulés de tous les fils du démon.
    pub fn reveils(&self) -> u64 {
        let mut n = 0;
        for t in fs::read_dir(format!("/proc/{}/task", self.pid())).unwrap().flatten() {
            let s = fs::read_to_string(t.path().join("status")).unwrap_or_default();
            for l in s.lines() {
                if l.starts_with("voluntary_ctxt_switches:") || l.starts_with("nonvoluntary_ctxt_switches:") {
                    n += l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                }
            }
        }
        n
    }

    pub fn signal(&self, sig: i32) {
        // SAFETY : kill(2) vers notre propre fils, encore à nous (pas ramassé).
        unsafe { libc::kill(self.pid() as i32, sig) };
    }

    /// Code de sortie dans le délai, sinon None.
    pub fn fin(&mut self, delai: Duration) -> Option<i32> {
        let t = Instant::now();
        while t.elapsed() < delai {
            if let Some(s) = self.enfant.try_wait().unwrap() {
                return s.code();
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Demon {
    fn drop(&mut self) {
        let _ = self.enfant.kill();
        let _ = self.enfant.wait();
    }
}
