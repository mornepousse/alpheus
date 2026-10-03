//! Boucle réelle : terminal, fil de travail, événements, outils externes.
//! Seule partie sans test unitaire : couverte par tests/premier_rendu.rs et
//! par l'essai réel de la tâche 16.

use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use super::App;
use crate::backends::Backends;
use crate::config::Config;
use crate::exec::SystemExecutor;
use crate::externes::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Délai maximal d'une commande système avant d'être tuée.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("alpheus")
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // SAFETY : getuid(2) ne peut pas échouer.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

pub fn build_app(dir: &Path, now: Instant) -> App {
    let (config, w_config) = Config::load(&dir.join("config.toml"));
    let (theme, w_theme) = Theme::load(&dir.join("colors.toml"));
    let (keymap, w_keys) = match Keymap::from_config(&config.keys) {
        Ok(k) => (k, None),
        Err(e) => (Keymap::default(), Some(format!("touches : {e}"))),
    };
    let mut app = App::new(config, theme, keymap, now);
    for w in [w_config, w_theme, w_keys].into_iter().flatten() {
        app.warn(w);
    }
    app
}

pub fn run() -> io::Result<()> {
    let app = build_app(&config_dir(), Instant::now());
    let exec = Arc::new(SystemExecutor::new(TOOL_TIMEOUT));
    let (tx, rx) = worker::spawn(Backends::system(exec.clone(), runtime_dir()));
    let mut terminal = ratatui::init(); // installe aussi la restauration en cas de panique
    execute!(io::stdout(), EnableMouseCapture)?;
    let res = event_loop(&mut terminal, app, &tx, &rx);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    exec.kill_running(); // aucun outil laissé derrière
    res
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App, tx: &Sender<Request>, rx: &Receiver<Update>) -> io::Result<()> {
    loop {
        for r in app.take_requests() {
            let _ = tx.send(r);
        }
        let mut hits = ui::HitMap::default();
        terminal.draw(|f| hits = ui::render(f, &app))?;
        if app.should_quit {
            return Ok(());
        }
        if let Some(ext) = app.take_external() {
            let r = externes::open(&mut CrosstermScreen, &mut ProcessLauncher, &app.external_argv(ext));
            terminal.clear()?;
            app.external_done(ext, r, Instant::now());
            continue;
        }
        if event::poll(Duration::from_millis(100))? {
            let now = Instant::now();
            match event::read()? {
                Event::Key(k) => app.on_key(k, now),
                Event::Mouse(m) => app.on_mouse(m, &hits, now),
                _ => {} // redimensionnement : redessiné au tour suivant
            }
        }
        while let Ok(u) = rx.try_recv() {
            app.on_update(u, Instant::now());
        }
        app.on_tick(Instant::now());
    }
}
