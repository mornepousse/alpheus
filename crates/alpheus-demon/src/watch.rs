//! Hot reload of the theme (themes spec § 3): one inotify instance on the config directory
//! (~/.config/alpheus), one fd in the event loop — no thread, no timer. A write, rename, creation
//! or removal of theme.toml, colors.toml or colors-light.toml re-resolves the roles once per loop
//! dispatch (the callback drains every queued record first), and every surface is repainted once,
//! only when the roles changed. While the directory does not exist, its parent is watched until it
//! appears; when it goes, the watch falls back to the parent. inotify unavailable: one line, and the
//! theme files are read at startup only. Read-only: nothing here writes to the directory.

use crate::inotify::{Events, Inotify};
use crate::log::write as log;
use crate::startup::describe;
use crate::wayland::App;
use alpheus_commun::paths;
use alpheus_commun::theme::settings::{
    BarStyle, DARK_FILE, LIGHT_FILE, ModeSetting, ModuleColors, Resolved, THEME_FILE, ThemeSettings, effective_mode, load_settings,
    resolve_detailed, resolve_dir,
};
use alpheus_commun::theme::{Mode, Roles};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode as Trigger, PostAction};
use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The files whose change re-resolves the theme.
pub const THEME_FILES: [&str; 3] = [THEME_FILE, DARK_FILE, LIGHT_FILE];

/// On the config directory: the spec's four events, plus the directory itself going away.
const DIR_MASK: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;
/// On its parent, while it does not exist: it being created or moved in.
const PARENT_MASK: u32 = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_ONLYDIR;
/// What the one watch descriptor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum On {
    Dir(i32),
    /// The directory does not exist: its parent, until it appears.
    Parent(i32),
    /// Neither could be watched again after the directory went: nothing more comes.
    Nothing,
}

/// What a drained batch of records means.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Batch {
    /// Re-resolve: a theme file changed, the directory came or went, or records were lost.
    pub theme: bool,
    /// The directory appeared (the parent was watched) or went (it was watched): watch anew.
    pub rearm: bool,
}

/// Whether one record of the directory touches a theme file. A regular file just created is still
/// empty and its IN_CLOSE_WRITE follows, so a creation counts only for a symlink (`ln -sf`,
/// home-manager), which never gets one.
pub(crate) fn touches_theme(mask: u32, name: &[u8], is_link: &dyn Fn(&[u8]) -> bool) -> bool {
    if mask & libc::IN_ISDIR != 0 || !THEME_FILES.iter().any(|f| f.as_bytes() == name) {
        return false;
    }
    mask & (libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_DELETE) != 0 || (mask & libc::IN_CREATE != 0 && is_link(name))
}

/// Every record of `buf` folded into one batch: a burst is one re-resolve.
pub(crate) fn scan(buf: &[u8], on: On, dir_name: &[u8], is_link: &dyn Fn(&[u8]) -> bool) -> Batch {
    let mut b = Batch::default();
    for e in Events(buf) {
        if e.mask & libc::IN_Q_OVERFLOW != 0 {
            b.theme = true; // records were lost: re-read to be sure
            continue;
        }
        match on {
            On::Dir(wd) if e.wd == wd => {
                if e.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                    b.theme = true;
                    b.rearm = true;
                } else if touches_theme(e.mask, e.name, is_link) {
                    b.theme = true;
                }
            }
            On::Parent(wd) if e.wd == wd && e.mask & libc::IN_ISDIR != 0 && e.name == dir_name => {
                b.theme = true;
                b.rearm = true;
            }
            _ => {}
        }
    }
    b
}

/// The lines of `now` that `before` did not have: a reload repeats no warning.
pub(crate) fn fresh<'a>(before: &'a [String], now: &'a [String]) -> impl Iterator<Item = &'a String> + 'a {
    now.iter().filter(move |w| !before.contains(w))
}

/// The inotify instance and its one watch.
pub(crate) struct ThemeWatch {
    fd: Inotify,
    dir: PathBuf,
    on: On,
}

impl ThemeWatch {
    /// inotify on `dir`, or on its parent while `dir` does not exist. Err: one line for the log.
    pub(crate) fn start(dir: &Path) -> Result<ThemeWatch, String> {
        let fd = Inotify::new()?;
        let on = arm(&fd, dir)?;
        Ok(ThemeWatch { fd, dir: dir.to_path_buf(), on })
    }

    /// A duplicate of the inotify fd for the event loop (the same instance: reading either drains both).
    pub(crate) fn watch_fd(&self) -> io::Result<OwnedFd> {
        self.fd.watch_fd()
    }

    /// Reads every queued record (until EAGAIN) and folds them into one batch; watches anew when
    /// the directory came or went.
    pub(crate) fn drain(&mut self) -> Batch {
        let mut batch = Batch::default();
        let name = self.dir.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        let dir = self.dir.clone();
        let is_link = move |f: &[u8]| is_symlink(&dir.join(OsStr::from_bytes(f)));
        self.fd.drain(|chunk| {
            let b = scan(chunk, self.on, &name, &is_link);
            batch.theme |= b.theme;
            batch.rearm |= b.rearm;
        });
        if batch.rearm {
            self.rearm();
        }
        batch
    }

    fn rearm(&mut self) {
        if let On::Dir(wd) | On::Parent(wd) = self.on {
            self.fd.rm(wd);
        }
        self.on = match arm(&self.fd, &self.dir) {
            Ok(on) => on,
            Err(e) => {
                log(&format!("theme: {e}; theme files no longer watched"));
                On::Nothing
            }
        };
    }
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// inotify follows a symlinked config directory to its target (the Nix store, with home-manager),
/// so replacing the link is never seen: one line at start says so.
fn symlink_line(dir: &Path) -> Option<String> {
    is_symlink(dir).then(|| format!("theme: {} is a symlink; replacing it is not seen until restart", dir.display()))
}

/// The directory, or its parent when the directory does not exist.
fn arm(fd: &Inotify, dir: &Path) -> Result<On, String> {
    match fd.add(dir, DIR_MASK) {
        Ok(wd) => Ok(On::Dir(wd)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = dir.parent().ok_or_else(|| format!("{} has no parent to wait in", dir.display()))?;
            fd.add(parent, PARENT_MASK).map(On::Parent).map_err(|e| format!("cannot watch {} ({e})", parent.display()))
        }
        Err(e) => Err(format!("cannot watch {} ({e})", dir.display())),
    }
}

/// Starts watching (`startup::run`, once the loop exists). Any failure: one line, the theme files read
/// at startup only, the bar unaffected.
pub(crate) fn start(state: &mut App, handle: &LoopHandle<'static, App>) {
    let dir = paths::config_dir();
    if let Some(line) = symlink_line(&dir) {
        log(&line);
    }
    let watch = match ThemeWatch::start(&dir) {
        Ok(w) => w,
        Err(e) => {
            log(&format!("theme: {e}; theme files read at startup only"));
            return;
        }
    };
    let fd = match watch.watch_fd() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("theme: could not watch the inotify fd ({e}); theme files read at startup only"));
            return;
        }
    };
    // Level-triggered: `drain` reads to EAGAIN, so the fd is never left readable at rest.
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
        app.theme_readable();
        Ok(PostAction::Continue)
    });
    if inserted.is_err() {
        log("theme: event loop refused the inotify source; theme files read at startup only");
        return;
    }
    state.theme.watch = Some(watch);
}

/// A reload repaints when the roles changed or the bar's shape did (module colours, style, opacity):
/// the roles can be equal while the bar's look is not (themes spec § 9, shapes spec § 4).
fn repaint_needed(new: &Roles, old: &Roles, shape_changed: bool) -> bool {
    shape_changed || new != old
}

fn colours_name(mc: ModuleColors) -> &'static str {
    match mc {
        ModuleColors::None => "none",
        ModuleColors::Icons => "icons",
        ModuleColors::Full => "full",
        ModuleColors::Pills => "pills",
    }
}

/// What a reload changes in the bar's shape (shapes spec § 4).
#[derive(Debug, PartialEq)]
pub(crate) struct ShapeChange {
    /// The module colours, the style, or the opacity under `translucent` changed: re-lay out and repaint.
    pub(crate) repaint: bool,
    /// To or from `floating`: every bar's layer surface is reconfigured before it paints again.
    pub(crate) placement: bool,
    /// What changed, for the reload's line: ", module colours full, style islands, opacity 0.6".
    pub(crate) note: String,
}

/// Compares the settings in effect with the ones just read. Allocates only on a reload.
pub(crate) fn shape_change(old: &ThemeSettings, new: &ThemeSettings) -> ShapeChange {
    let mut note = String::new();
    if old.module_colors != new.module_colors {
        note.push_str(", module colours ");
        note.push_str(colours_name(new.module_colors));
    }
    if old.style != new.style {
        note.push_str(", style ");
        note.push_str(new.style.name());
    }
    // Only `translucent` reads the opacity: under any other style a new value changes no pixel.
    if new.style == BarStyle::Translucent && old.opacity != new.opacity {
        note.push_str(&format!(", opacity {}", new.opacity));
    }
    let floating = |s: &ThemeSettings| s.style == BarStyle::Floating;
    ShapeChange { repaint: !note.is_empty(), placement: floating(old) != floating(new), note }
}

/// What a reload resolved.
pub(crate) struct Reload {
    pub(crate) settings: ThemeSettings,
    pub(crate) resolved: Resolved,
    /// theme.toml is empty and the settings in effect are not what an empty file means: kept.
    pub(crate) kept: bool,
}

/// `resolve_dir` for a reload. When theme.toml exists but holds nothing (empty or whitespace) — an
/// editor truncating it mid-save, or left empty on purpose — the settings in effect (`previous`)
/// stay, so a floating bar never flips to flat and back; the colour files and the portal's word
/// still apply (a new wallpaper, a dark/light switch). Startup does not go through here: an empty
/// file there is the defaults.
pub(crate) fn reload_settings(dir: &Path, previous: &ThemeSettings, portal: impl FnOnce() -> Option<Mode>) -> Reload {
    let blank = matches!(std::fs::read(dir.join(THEME_FILE)), Ok(text) if text.iter().all(u8::is_ascii_whitespace));
    if !blank {
        let (settings, resolved) = resolve_dir(dir, portal);
        return Reload { settings, resolved, kept: false };
    }
    // `portal` is asked only in mode auto, as `resolve_dir` does.
    let portal = if previous.mode == ModeSetting::Auto { portal() } else { None };
    let resolved = resolve_detailed(previous, effective_mode(previous.mode, portal), dir);
    let kept = load_settings(dir).0 != *previous;
    Reload { settings: previous.clone(), resolved, kept }
}

impl App {
    /// The inotify fd is readable (calloop callback): every queued record drained, one re-resolve at most.
    pub(crate) fn theme_readable(&mut self) {
        let Some(watch) = self.theme.watch.as_mut() else { return };
        if watch.drain().theme {
            self.theme_changed();
        }
    }

    /// Re-reads theme.toml and the colour files and resolves them for the mode in effect (the
    /// portal's word in mode auto). Every surface is repainted once when the roles
    /// changed; only warnings not logged at the last resolve are logged. A wallpaper colour file
    /// that exists but cannot be used (matugen caught mid-write) keeps the roles on screen:
    /// the bar never flashes to the default theme on a reload.
    pub(crate) fn theme_changed(&mut self) {
        let dir = paths::config_dir();
        let portal = self.theme.portal_mode;
        let Reload { settings, resolved, kept } = reload_settings(&dir, &self.theme.settings, || portal);
        if kept && !self.theme.blank_noted {
            self.theme.blank_noted = true;
            log("theme: theme.toml is empty, keeping the current settings until it is written");
        }
        let mode = effective_mode(settings.mode, portal);
        let warnings = resolved.lines();
        let mut new_lines = false;
        for w in fresh(&self.theme.warnings, &warnings) {
            log(w);
            new_lines = true;
        }
        self.theme.warnings = warnings;
        let shape = shape_change(&self.theme.settings, &settings);
        self.bar.module_colors = settings.module_colors;
        // The layout's spacing and the placement follow the bar's style: set before
        // any repaint or re-placement below.
        self.bar.style = settings.style;
        self.theme.settings = settings;
        // A switch to mode = "auto" starts following the portal (a no-op once tried, or when not
        // auto). After the store, never before: it reads `self.theme.settings`.
        self.follow_portal_if_auto();
        // The mode file for the TUI follows the mode in effect (a no-op when it did not change).
        self.publish_mode();
        if resolved.bad_file {
            if new_lines {
                log("theme: keeping the current colours until the colour file is usable");
            }
            // The shape is read from theme.toml, not from the colour file: it still applies.
            if shape.repaint {
                log(&format!("theme reloaded: {}", shape.note.trim_start_matches(", ")));
                self.restyle(shape.placement);
            }
            return;
        }
        if !repaint_needed(&resolved.roles, &self.roles, shape.repaint) {
            return;
        }
        self.roles = resolved.roles;
        log(&format!("theme reloaded: {}{}", describe(&self.theme.settings, mode), shape.note));
        self.restyle(shape.placement);
    }

    /// Every surface once, with the current roles: the bars, an open popup, the shown OSD pill, the
    /// launcher box.
    pub(crate) fn repaint_all(&mut self) {
        self.bars_repaint();
        self.popup_repaint();
        self.osd_repaint();
        let _ = self.launcher_guarded("theme", App::launcher_repaint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inotify::{Event, HEADER};
    use alpheus_commun::theme::settings::THEME_FILE;

    #[test]
    fn a_reload_repaints_when_only_the_module_colours_changed() {
        let r = Roles::default();
        assert!(!repaint_needed(&r, &r, false), "nothing changed: no repaint");
        assert!(repaint_needed(&r, &r, true), "equal roles, other module colours: repaint");
        let other = Roles { fg: alpheus_commun::theme::Rgb(1, 2, 3), ..Roles::default() };
        assert!(repaint_needed(&other, &r, false), "other roles: repaint");
        assert_eq!(
            [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills].map(colours_name),
            ["none", "icons", "full", "pills"]
        );
    }

    use alpheus_commun::theme::settings::{BarStyle, ThemeSettings};

    fn shaped(style: BarStyle, opacity: f32, module_colors: ModuleColors) -> ThemeSettings {
        ThemeSettings { style, opacity, module_colors, ..ThemeSettings::default() }
    }

    #[test]
    fn a_reload_restyles_on_a_new_style_opacity_or_module_colours() {
        let flat = shaped(BarStyle::Flat, 0.8, ModuleColors::Icons);
        let same = shape_change(&flat, &flat.clone());
        assert_eq!((same.repaint, same.placement, same.note.as_str()), (false, false, ""), "nothing changed");
        let c = shape_change(&flat, &shaped(BarStyle::Islands, 0.8, ModuleColors::Icons));
        assert_eq!((c.repaint, c.placement, c.note.as_str()), (true, false, ", style islands"));
        let c = shape_change(&flat, &shaped(BarStyle::Flat, 0.8, ModuleColors::Pills));
        assert_eq!((c.repaint, c.placement, c.note.as_str()), (true, false, ", module colours pills"));
        let translucent = shaped(BarStyle::Translucent, 0.8, ModuleColors::Icons);
        let c = shape_change(&translucent, &shaped(BarStyle::Translucent, 0.5, ModuleColors::Icons));
        assert_eq!((c.repaint, c.placement, c.note.as_str()), (true, false, ", opacity 0.5"));
        let c = shape_change(&flat, &shaped(BarStyle::Flat, 0.5, ModuleColors::Icons));
        assert_eq!((c.repaint, c.note.as_str()), (false, ""), "outside translucent the opacity changes no pixel");
        let c = shape_change(&flat, &shaped(BarStyle::Translucent, 0.5, ModuleColors::Full));
        assert_eq!(c.note, ", module colours full, style translucent, opacity 0.5", "every change, in a fixed order");
    }

    #[test]
    fn only_a_switch_to_or_from_floating_reconfigures_the_surfaces() {
        use BarStyle::*;
        let s = |style| shaped(style, 0.8, ModuleColors::Icons);
        assert!(shape_change(&s(Flat), &s(Floating)).placement, "flat -> floating");
        assert!(shape_change(&s(Floating), &s(Islands)).placement, "floating -> islands");
        for (a, b) in [(Flat, Islands), (Islands, Powerline), (Underline, Translucent), (Translucent, Flat), (Floating, Floating)] {
            assert!(!shape_change(&s(a), &s(b)).placement, "{a:?} -> {b:?} keeps the placement");
        }
    }

    /// An editor that truncates theme.toml before writing it hands a reload an empty file: the
    /// settings in effect stay (a floating bar never flips to flat and back, closing a popup), and
    /// the next real file applies. A missing theme.toml is not empty: its defaults apply.
    #[test]
    fn an_empty_theme_toml_during_a_reload_keeps_the_settings() {
        let dir = temp("blank");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(THEME_FILE);
        let floating = ThemeSettings { style: BarStyle::Floating, ..ThemeSettings::default() };
        for blank in ["", " \n\t\n"] {
            std::fs::write(&path, blank).unwrap();
            let r = reload_settings(&dir, &floating, || None);
            assert_eq!((r.settings.style, r.kept), (BarStyle::Floating, true), "{blank:?}: kept");
        }
        std::fs::write(&path, "style = \"islands\"\n").unwrap();
        let r = reload_settings(&dir, &floating, || None);
        assert_eq!((r.settings.style, r.kept), (BarStyle::Islands, false), "a real file applies");
        std::fs::remove_file(&path).unwrap();
        let r = reload_settings(&dir, &floating, || None);
        assert_eq!((r.settings.style, r.kept), (BarStyle::Flat, false), "no file: the defaults apply");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty theme.toml left on purpose (the defaults) keeps only the settings: the colour files
    /// are read again (matugen's next wallpaper repaints), and nothing worth a line is kept when the
    /// settings in effect are what an empty file means (startup's catch-up resolve logs nothing).
    #[test]
    fn an_empty_theme_toml_still_reads_the_colour_files() {
        let dir = temp("blank-colours");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(THEME_FILE), "").unwrap();
        let legacy = |bg: &str| {
            format!(
                "bg = \"{bg}\"\nfg = \"#e0e0e0\"\naccent = \"#ffaa00\"\nmuted = \"#808080\"\nsurface = \"#203040\"\nerror = \"#ff5555\"\nok = \"#55ff55\"\n"
            )
        };
        std::fs::write(dir.join(DARK_FILE), legacy("#102030")).unwrap();
        // What startup resolved: an empty file with a colors.toml is the wallpaper (spec § 2).
        let (startup, _) = resolve_dir(&dir, || None);
        let r = reload_settings(&dir, &startup, || None);
        assert_eq!(r.resolved.roles.bar_bg, alpheus_commun::theme::Rgb(0x10, 0x20, 0x30));
        assert!(!r.kept, "the settings in effect are the empty file's: nothing to say");
        std::fs::write(dir.join(DARK_FILE), legacy("#405060")).unwrap();
        let r = reload_settings(&dir, &startup, || None);
        assert_eq!(r.resolved.roles.bar_bg, alpheus_commun::theme::Rgb(0x40, 0x50, 0x60), "the new wallpaper");
        assert_eq!(r.settings, startup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The portal's word still applies under an empty theme.toml in mode auto (its default).
    #[test]
    fn an_empty_theme_toml_still_follows_the_portal() {
        use alpheus_commun::theme::presets::preset;
        let dir = temp("blank-portal");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(THEME_FILE), "").unwrap();
        let auto = ThemeSettings::default();
        let catppuccin = preset("catppuccin").unwrap();
        let r = reload_settings(&dir, &auto, || Some(Mode::Light));
        assert_eq!(r.resolved.roles, catppuccin.light.unwrap(), "light");
        let r = reload_settings(&dir, &auto, || Some(Mode::Dark));
        assert_eq!(r.resolved.roles, catppuccin.dark, "dark");
        let dark = ThemeSettings { mode: alpheus_commun::theme::settings::ModeSetting::Dark, ..auto };
        let r = reload_settings(&dir, &dark, || panic!("a fixed mode never asks"));
        assert_eq!(r.resolved.roles, catppuccin.dark);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn no_link(_: &[u8]) -> bool {
        false
    }

    fn all_links(_: &[u8]) -> bool {
        true
    }

    /// One raw record as the kernel lays it out: the name NUL-padded to a multiple of 16.
    fn record(wd: i32, mask: u32, name: &str) -> Vec<u8> {
        let len = if name.is_empty() { 0 } else { (name.len() / 16 + 1) * 16 };
        let mut r = Vec::new();
        r.extend_from_slice(&wd.to_ne_bytes());
        r.extend_from_slice(&mask.to_ne_bytes());
        r.extend_from_slice(&0u32.to_ne_bytes());
        r.extend_from_slice(&(len as u32).to_ne_bytes());
        r.extend_from_slice(name.as_bytes());
        r.resize(HEADER + len, 0);
        r
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-watch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn records_are_parsed_and_a_truncated_tail_dropped() {
        let mut buf = record(1, libc::IN_CLOSE_WRITE, "theme.toml");
        buf.extend(record(1, libc::IN_DELETE_SELF, ""));
        buf.extend(&record(1, libc::IN_CREATE, "colors.toml")[..20]);
        let events: Vec<Event> = Events(&buf).collect();
        assert_eq!(
            events,
            [
                Event { wd: 1, mask: libc::IN_CLOSE_WRITE, name: &b"theme.toml"[..] },
                Event { wd: 1, mask: libc::IN_DELETE_SELF, name: &b""[..] }
            ]
        );
        assert_eq!(Events(&[0u8; 7]).count(), 0, "shorter than a header: nothing, no panic");
    }

    #[test]
    fn only_the_three_theme_files_count() {
        for f in THEME_FILES {
            for mask in [libc::IN_CLOSE_WRITE, libc::IN_MOVED_TO, libc::IN_DELETE] {
                assert!(touches_theme(mask, f.as_bytes(), &no_link), "{f} {mask:#x}");
            }
        }
        for other in ["bar.toml", "launcher.toml", "theme.toml.tmp", "colors.toml~", ".theme.toml.swp", ""] {
            assert!(!touches_theme(libc::IN_CLOSE_WRITE, other.as_bytes(), &all_links), "{other}");
        }
        assert!(!touches_theme(libc::IN_CREATE | libc::IN_ISDIR, b"theme.toml", &all_links), "a directory by that name");
    }

    #[test]
    fn a_new_regular_file_waits_for_its_close_write_but_a_new_link_counts() {
        assert!(!touches_theme(libc::IN_CREATE, b"theme.toml", &no_link), "still empty: its IN_CLOSE_WRITE follows");
        assert!(touches_theme(libc::IN_CREATE, b"theme.toml", &all_links), "ln -sf, home-manager: no IN_CLOSE_WRITE ever comes");
    }

    #[test]
    fn a_burst_folds_into_one_batch() {
        let mut buf = Vec::new();
        for i in 0..20 {
            buf.extend(record(3, libc::IN_CLOSE_WRITE, THEME_FILES[i % 3]));
            buf.extend(record(3, libc::IN_CLOSE_WRITE, "bar.toml"));
        }
        assert_eq!(scan(&buf, On::Dir(3), b"alpheus", &no_link), Batch { theme: true, rearm: false });
        let unrelated: Vec<u8> = (0..20).flat_map(|_| record(3, libc::IN_CLOSE_WRITE, "launcher.toml")).collect();
        assert_eq!(scan(&unrelated, On::Dir(3), b"alpheus", &no_link), Batch::default());
        let other_watch = record(9, libc::IN_CLOSE_WRITE, "theme.toml");
        assert_eq!(scan(&other_watch, On::Dir(3), b"alpheus", &no_link), Batch::default(), "another watch's record");
        assert!(scan(&record(-1, libc::IN_Q_OVERFLOW, ""), On::Dir(3), b"alpheus", &no_link).theme, "records lost: re-read");
    }

    #[test]
    fn the_directory_appearing_or_going_rearms() {
        let p = On::Parent(2);
        let both = Batch { theme: true, rearm: true };
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "alpheus"), p, b"alpheus", &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_MOVED_TO | libc::IN_ISDIR, "alpheus"), p, b"alpheus", &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "other"), p, b"alpheus", &no_link), Batch::default());
        assert_eq!(scan(&record(2, libc::IN_CREATE, "alpheus"), p, b"alpheus", &no_link), Batch::default(), "a file named alpheus");
        for gone in [libc::IN_DELETE_SELF, libc::IN_MOVE_SELF, libc::IN_IGNORED] {
            assert_eq!(scan(&record(4, gone, ""), On::Dir(4), b"alpheus", &no_link), both, "{gone:#x}");
        }
    }

    #[test]
    fn a_reload_logs_only_new_warnings() {
        let before = vec!["a".to_string(), "b".to_string()];
        let now = vec!["b".to_string(), "c".to_string()];
        assert_eq!(fresh(&before, &now).collect::<Vec<_>>(), [&"c".to_string()]);
        assert_eq!(fresh(&now, &now).count(), 0, "the same warnings again: nothing");
    }

    /// The kernel merges identical successive records, so the burst alternates the three files:
    /// 200 records of 32 bytes, more than one 4 KB read.
    #[test]
    fn a_burst_bigger_than_one_read_drains_in_one_call() {
        let d = temp("burst");
        std::fs::create_dir_all(&d).unwrap();
        let mut w = ThemeWatch::start(&d).unwrap();
        assert!(matches!(w.on, On::Dir(_)), "{:?}", w.on);
        assert_eq!(w.drain(), Batch::default(), "nothing yet");
        for i in 0..200 {
            std::fs::write(d.join(THEME_FILES[i % 3]), format!("# {i}\n")).unwrap();
        }
        assert!(w.drain().theme, "the burst seen");
        assert_eq!(w.drain(), Batch::default(), "wholly drained: nothing left for a second dispatch");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_directory_is_awaited_in_its_parent_and_followed_when_it_goes() {
        let parent = temp("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let d = parent.join("alpheus");
        let mut w = ThemeWatch::start(&d).unwrap();
        assert!(matches!(w.on, On::Parent(_)), "{:?}", w.on);
        std::fs::create_dir(parent.join("other")).unwrap();
        assert_eq!(w.drain(), Batch::default(), "another directory: nothing");
        std::fs::create_dir(&d).unwrap();
        assert_eq!(w.drain(), Batch { theme: true, rearm: true }, "it appeared");
        assert!(matches!(w.on, On::Dir(_)), "{:?}", w.on);
        std::fs::write(d.join("theme.toml.tmp"), "theme = \"nord\"\n").unwrap();
        std::fs::rename(d.join("theme.toml.tmp"), d.join(THEME_FILE)).unwrap();
        assert!(w.drain().theme, "renamed into place");
        std::fs::remove_file(d.join(THEME_FILE)).unwrap();
        std::fs::remove_dir(&d).unwrap();
        assert_eq!(w.drain(), Batch { theme: true, rearm: true }, "it went");
        assert!(matches!(w.on, On::Parent(_)), "back to waiting in the parent: {:?}", w.on);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn an_unwatchable_directory_is_one_line_never_a_panic() {
        let Err(e) = ThemeWatch::start(Path::new("/nonexistent-alpheus-parent/alpheus")) else {
            panic!("neither the directory nor its parent exists")
        };
        assert!(e.starts_with("cannot watch /nonexistent-alpheus-parent"), "{e}");
    }

    #[test]
    fn a_symlinked_config_directory_is_announced() {
        let real = temp("link-real");
        std::fs::create_dir_all(&real).unwrap();
        let link = temp("link-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let line = symlink_line(&link).expect("a symlink");
        assert!(line.contains("is a symlink; replacing it is not seen until restart") && line.contains("link-link"), "{line}");
        assert_eq!(symlink_line(&real), None, "a plain directory: silent");
        assert_eq!(symlink_line(&temp("link-absent")), None, "an absent one: silent");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&real);
    }
}
