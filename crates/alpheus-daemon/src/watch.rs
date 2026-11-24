//! Hot reload of the theme (themes spec § 3): one inotify instance on the config directory
//! (~/.config/alpheus), one fd in the event loop — no thread, no timer. A write, rename, creation
//! or removal of theme.toml, colors.toml or colors-light.toml re-resolves the roles once per loop
//! dispatch (the callback drains every queued record first), and every surface is repainted once,
//! only when the roles changed. While the directory does not exist, its parent is watched until it
//! appears; when it goes, the watch falls back to the parent. inotify unavailable: one line, and the
//! theme files are read at startup only. Read-only: nothing here writes to the directory.
//!
//! It carries the Shield's flag too: a second watch descriptor in the same instance, so the mode
//! shows at once when something outside alpheus writes the flag, at no cost in threads or timers.
//!
//! Never:
//! - write, create or remove anything: the watch only watches
//!   (`the_theme_and_camera_watches_only_read`);
//! - make a second inotify instance for the Shield (`the_shield_watch_is_the_theme_watchs_instance`);
//! - name a path in a log line: the Shield's lines here are fixed words
//!   (`the_shield_logs_name_nothing`);
//! - keep a timer or wake the loop at rest: the descriptor is level-triggered and drained
//!   (`nothing_resident_while_the_launcher_is_closed`, and the themes' and Shield's
//!   `at_rest_no_wakeups_*` tests).
//!
//! See: docs/features/themes.md ("A change applies at once", and the symlinked configuration
//! directory), docs/features/shield.md ("Without a watch"), docs/decisions.md ("The Shield flag is
//! watched in the theme watch's inotify instance").

use crate::inotify::{Events, Inotify};
use crate::log::write as log;
use crate::startup::describe;
use crate::wayland::App;
use alpheus_common::paths;
use alpheus_common::theme::settings::{
    BarStyle, DARK_FILE, LIGHT_FILE, ModeSetting, ModuleColors, Resolved, THEME_FILE, ThemeSettings, effective_mode, load_settings,
    resolve_detailed, resolve_dir,
};
use alpheus_common::theme::{Mode, Roles};
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
/// On the Shield flag's directory (shield spec § 3): the flag's name appearing, going or being
/// renamed, and the directory itself going away.
const FLAG_DIR_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_TO
    | libc::IN_MOVED_FROM
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;
/// What the one watch descriptor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum On {
    Dir(i32),
    /// The directory does not exist: its parent, until it appears.
    Parent(i32),
    /// Neither could be watched again after the directory went: nothing more comes.
    Nothing,
}

impl On {
    fn wd(self) -> Option<i32> {
        match self {
            On::Dir(wd) | On::Parent(wd) => Some(wd),
            On::Nothing => None,
        }
    }
}

/// What a drained batch of records means.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Batch {
    /// Re-resolve: a theme file changed, the directory came or went, or records were lost.
    pub theme: bool,
    /// The directory appeared (the parent was watched) or went (it was watched): watch anew.
    pub rearm: bool,
    /// Re-read the Shield flag: its name changed in its directory, the directory came or went, or
    /// records were lost.
    pub shield: bool,
    /// The flag's directory appeared or went: watch it anew.
    pub shield_rearm: bool,
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

/// Whether one record of the flag's directory is about the flag's name: any creation, removal or
/// rename of it (its content never matters, only its existence).
pub(crate) fn touches_flag(mask: u32, name: &[u8], flag_name: &[u8]) -> bool {
    mask & libc::IN_ISDIR == 0
        && name == flag_name
        && mask & (libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM) != 0
}

/// The theme target of a scan: what it is on and the directory's own name.
pub(crate) type ThemeScan<'a> = (On, &'a [u8]);
/// The shield target of a scan: what it is on, the directory's name and the flag's name.
pub(crate) type ShieldScan<'a> = (On, &'a [u8], &'a [u8]);

/// Every record of `buf` folded into one batch: a burst is one re-resolve and one re-read.
pub(crate) fn scan(buf: &[u8], theme: Option<ThemeScan>, shield: Option<ShieldScan>, is_link: &dyn Fn(&[u8]) -> bool) -> Batch {
    let mut b = Batch::default();
    for e in Events(buf) {
        if e.mask & libc::IN_Q_OVERFLOW != 0 {
            // records were lost: re-read to be sure
            b.theme |= theme.is_some();
            b.shield |= shield.is_some();
            continue;
        }
        match theme {
            Some((On::Dir(wd), _)) if e.wd == wd => {
                if e.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                    b.theme = true;
                    b.rearm = true;
                } else if touches_theme(e.mask, e.name, is_link) {
                    b.theme = true;
                }
            }
            Some((On::Parent(wd), dir_name)) if e.wd == wd && e.mask & libc::IN_ISDIR != 0 && e.name == dir_name => {
                b.theme = true;
                b.rearm = true;
            }
            _ => {}
        }
        match shield {
            Some((On::Dir(wd), _, flag_name)) if e.wd == wd => {
                if e.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                    b.shield = true;
                    b.shield_rearm = true;
                } else if touches_flag(e.mask, e.name, flag_name) {
                    b.shield = true;
                }
            }
            Some((On::Parent(wd), dir_name, _)) if e.wd == wd && e.mask & libc::IN_ISDIR != 0 && e.name == dir_name => {
                b.shield = true;
                b.shield_rearm = true;
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

/// One watched directory: what its descriptor is on.
struct Target {
    dir: PathBuf,
    on: On,
}

impl Target {
    fn dir_name(&self) -> Vec<u8> {
        self.dir.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default()
    }
}

/// The Shield flag's directory and the flag's name in it.
struct ShieldTarget {
    target: Target,
    flag_name: Vec<u8>,
}

/// The one inotify instance and its watches: the config directory (the theme files) and, with
/// `[shield]` configured, the Shield flag's directory (shield spec § 3). Each target arms and
/// fails on its own.
pub(crate) struct Watches {
    fd: Inotify,
    theme: Option<Target>,
    shield: Option<ShieldTarget>,
}

impl Watches {
    /// inotify on `theme_dir` (or on its parent while it does not exist) and, with a `flag`, on the
    /// flag's directory. Err only when inotify itself is unavailable; a target that cannot be
    /// armed logs its own line and is left out, the other unaffected.
    pub(crate) fn start(theme_dir: &Path, flag: Option<&Path>) -> Result<Watches, String> {
        let fd = Inotify::new()?;
        let theme = match arm(&fd, theme_dir, DIR_MASK, None) {
            Ok(on) => Some(Target { dir: theme_dir.to_path_buf(), on }),
            Err(e) => {
                log(&format!("theme: {e}; theme files read at startup only"));
                None
            }
        };
        let shield = flag.and_then(|flag| {
            let (dir, name) = (flag.parent()?, flag.file_name()?);
            match arm(&fd, dir, FLAG_DIR_MASK, theme.as_ref().and_then(|t| t.on.wd())) {
                Ok(on) => Some(ShieldTarget { target: Target { dir: dir.to_path_buf(), on }, flag_name: name.as_bytes().to_vec() }),
                Err(e) => {
                    log(&shield_unwatched_line(&e));
                    None
                }
            }
        });
        let mut w = Watches { fd, theme, shield };
        w.settle();
        Ok(w)
    }

    /// Two descriptors that are one (an alias the paths do not show): inotify keeps one watch per
    /// inode, so the second `add` replaced the first one's mask. The shield target is dropped, the
    /// theme's mask put back, and the degraded mode applies.
    fn settle(&mut self) {
        let shared = match (&self.theme, &self.shield) {
            (Some(t), Some(s)) => t.on.wd().is_some() && t.on.wd() == s.target.on.wd(),
            _ => false,
        };
        if !shared {
            return;
        }
        self.shield = None;
        let restored = self.theme.as_ref().map(|t| {
            let (path, mask) = match t.on {
                On::Parent(_) => (t.dir.parent().unwrap_or(&t.dir), PARENT_MASK),
                _ => (t.dir.as_path(), DIR_MASK),
            };
            self.fd.add(path, mask).is_ok()
        });
        if restored == Some(false) {
            // The theme's directory went in between: wait for it again as at any loss.
            log("theme: mask not restored; watching again");
            self.rearm_theme();
        }
        log("shield: flag not watched (shares the config directory's watch)");
    }

    pub(crate) fn shield_armed(&self) -> bool {
        self.shield.as_ref().is_some_and(|s| s.target.on != On::Nothing)
    }

    #[cfg(test)]
    fn theme_on(&self) -> On {
        self.theme.as_ref().map_or(On::Nothing, |t| t.on)
    }

    #[cfg(test)]
    fn shield_on(&self) -> On {
        self.shield.as_ref().map_or(On::Nothing, |t| t.target.on)
    }

    pub(crate) fn theme_armed(&self) -> bool {
        self.theme.as_ref().is_some_and(|t| t.on != On::Nothing)
    }

    /// A duplicate of the inotify fd for the event loop (the same instance: reading either drains both).
    pub(crate) fn watch_fd(&self) -> io::Result<OwnedFd> {
        self.fd.watch_fd()
    }

    /// Reads every queued record (until EAGAIN) and folds them into one batch; watches anew the
    /// directory that came or went.
    pub(crate) fn drain(&mut self) -> Batch {
        let mut batch = Batch::default();
        let theme_name = self.theme.as_ref().map(Target::dir_name);
        let theme_dir = self.theme.as_ref().map(|t| t.dir.clone());
        let is_link = move |f: &[u8]| theme_dir.as_ref().is_some_and(|d| is_symlink(&d.join(OsStr::from_bytes(f))));
        let theme_scan = self.theme.as_ref().zip(theme_name.as_deref()).map(|(t, n)| (t.on, n));
        let shield_name = self.shield.as_ref().map(|s| s.target.dir_name());
        let shield_scan = self.shield.as_ref().zip(shield_name.as_deref()).map(|(s, n)| (s.target.on, n, s.flag_name.as_slice()));
        self.fd.drain(|chunk| {
            let b = scan(chunk, theme_scan, shield_scan, &is_link);
            batch.theme |= b.theme;
            batch.rearm |= b.rearm;
            batch.shield |= b.shield;
            batch.shield_rearm |= b.shield_rearm;
        });
        if batch.rearm {
            self.rearm_theme();
        }
        if batch.shield_rearm {
            self.rearm_shield();
        }
        if batch.rearm || batch.shield_rearm {
            self.settle();
        }
        batch
    }

    fn rearm_theme(&mut self) {
        let keep = self.shield.as_ref().and_then(|s| s.target.on.wd());
        let Some(t) = self.theme.as_mut() else { return };
        if let Some(wd) = t.on.wd() {
            self.fd.rm(wd);
        }
        t.on = match arm(&self.fd, &t.dir, DIR_MASK, keep) {
            Ok(on) => on,
            Err(e) => {
                log(&format!("theme: {e}; theme files no longer watched"));
                On::Nothing
            }
        };
    }

    fn rearm_shield(&mut self) {
        let keep = self.theme.as_ref().and_then(|t| t.on.wd());
        let Some(s) = self.shield.as_mut() else { return };
        if let Some(wd) = s.target.on.wd() {
            self.fd.rm(wd);
        }
        s.target.on = match arm(&self.fd, &s.target.dir, FLAG_DIR_MASK, keep) {
            Ok(on) => on,
            Err(e) => {
                log(&shield_unwatched_line(&e));
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

/// Why a target could not be armed: `text` names the path (the theme's lines keep it), `reason`
/// is the OS reason alone (the Shield's line never carries a path: the flag's can name the user).
#[derive(Debug)]
struct ArmError {
    text: String,
    reason: String,
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The Shield's log line for a flag that cannot be watched: the reason, never a path.
fn shield_unwatched_line(e: &ArmError) -> String {
    format!("shield: flag not watched ({})", e.reason)
}

/// The directory (with `mask`), or its parent when the directory does not exist.
fn arm(fd: &Inotify, dir: &Path, mask: u32, keep: Option<i32>) -> Result<On, ArmError> {
    arm_with(fd, dir, mask, keep, || {})
}

/// `arm`, with `between` run after the directory was found missing and before its parent is watched:
/// the window a test fills by creating the directory.
fn arm_with(fd: &Inotify, dir: &Path, mask: u32, keep: Option<i32>, between: impl FnOnce()) -> Result<On, ArmError> {
    match fd.add(dir, mask) {
        Ok(wd) => Ok(On::Dir(wd)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let Some(parent) = dir.parent() else {
                return Err(ArmError {
                    text: format!("{} has no parent to wait in", dir.display()),
                    reason: "no parent directory to wait in".into(),
                });
            };
            between();
            let parent_wd = fd
                .add(parent, PARENT_MASK)
                .map_err(|e| ArmError { text: format!("cannot watch {} ({e})", parent.display()), reason: e.kind().to_string() })?;
            // The directory may have appeared between the two calls, before the parent could tell:
            // looked for once more now that the parent is watched, so its creation is never missed. Once
            // found (or refused), the parent's watch is dropped, unless its descriptor is the other
            // target's (inotify gives one per inode).
            let drop_parent = || {
                if keep != Some(parent_wd) {
                    fd.rm(parent_wd);
                }
            };
            match fd.add(dir, mask) {
                Ok(wd) => {
                    drop_parent();
                    Ok(On::Dir(wd))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(On::Parent(parent_wd)),
                // Anything else cannot be waited out: an error, never a silent wait in the parent.
                Err(e) => {
                    drop_parent();
                    Err(ArmError { text: format!("cannot watch {} ({e})", dir.display()), reason: e.kind().to_string() })
                }
            }
        }
        Err(e) => Err(ArmError { text: format!("cannot watch {} ({e})", dir.display()), reason: e.kind().to_string() }),
    }
}

/// The flag to watch: the configured one, when Shield mode is on.
fn flag_to_watch(shield: Option<&crate::shield::app::ShieldApp>) -> Option<PathBuf> {
    shield.map(|s| s.flag().to_path_buf())
}

/// Starts watching (`startup::run`, once the loop exists). Any failure: one line, the theme files
/// read at startup only (or the flag re-read in the degraded mode), the bar unaffected. The Shield
/// flag, when configured, is read once the watch is armed (nothing between the read and the arm
/// is missed).
pub(crate) fn start(state: &mut App, handle: &LoopHandle<'static, App>) {
    let dir = paths::config_dir();
    if let Some(line) = symlink_line(&dir) {
        log(&line);
    }
    let flag = flag_to_watch(state.shield.as_ref());
    let watch = match Watches::start(&dir, flag.as_deref()) {
        Ok(w) => Some(w),
        Err(e) => {
            log(&format!("theme: {e}; theme files read at startup only"));
            if flag.is_some() {
                log(&format!("shield: flag not watched ({e})"));
            }
            None
        }
    };
    let watch = watch.and_then(|w| attach(handle, w));
    state.theme.watch = watch;
    state.shield_start();
}

/// The loop source for the instance, when either target is armed.
fn attach(handle: &LoopHandle<'static, App>, w: Watches) -> Option<Watches> {
    if !w.theme_armed() && !w.shield_armed() {
        return None;
    }
    let fd = match w.watch_fd() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("theme: could not watch the inotify fd ({e}); theme files read at startup only"));
            return None;
        }
    };
    // Level-triggered: `drain` reads to EAGAIN, so the fd is never left readable at rest.
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
        app.watch_readable();
        Ok(PostAction::Continue)
    });
    if inserted.is_err() {
        log("theme: event loop refused the inotify source; theme files read at startup only");
        return None;
    }
    Some(w)
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
    /// The inotify fd is readable (calloop callback): every queued record drained once, then one
    /// re-resolve of the theme and one re-read of the Shield flag at most.
    pub(crate) fn watch_readable(&mut self) {
        let Some(watch) = self.theme.watch.as_mut() else { return };
        let batch = watch.drain();
        if batch.theme {
            self.theme_changed();
        }
        if batch.shield {
            self.shield_readable();
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
    /// launcher box, the quick notepad's box.
    pub(crate) fn repaint_all(&mut self) {
        self.bars_repaint();
        self.popup_repaint();
        self.osd_repaint();
        let _ = self.launcher_guarded("theme", App::launcher_repaint);
        let _ = self.notes_guarded("theme", App::notes_repaint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inotify::{Event, HEADER};
    use alpheus_common::theme::settings::THEME_FILE;

    #[test]
    fn a_reload_repaints_when_only_the_module_colours_changed() {
        let r = Roles::default();
        assert!(!repaint_needed(&r, &r, false), "nothing changed: no repaint");
        assert!(repaint_needed(&r, &r, true), "equal roles, other module colours: repaint");
        let other = Roles { fg: alpheus_common::theme::Rgb(1, 2, 3), ..Roles::default() };
        assert!(repaint_needed(&other, &r, false), "other roles: repaint");
        assert_eq!(
            [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills].map(colours_name),
            ["none", "icons", "full", "pills"]
        );
    }

    use alpheus_common::theme::settings::{BarStyle, ThemeSettings};

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
        assert_eq!(r.resolved.roles.bar_bg, alpheus_common::theme::Rgb(0x10, 0x20, 0x30));
        assert!(!r.kept, "the settings in effect are the empty file's: nothing to say");
        std::fs::write(dir.join(DARK_FILE), legacy("#405060")).unwrap();
        let r = reload_settings(&dir, &startup, || None);
        assert_eq!(r.resolved.roles.bar_bg, alpheus_common::theme::Rgb(0x40, 0x50, 0x60), "the new wallpaper");
        assert_eq!(r.settings, startup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The portal's word still applies under an empty theme.toml in mode auto (its default).
    #[test]
    fn an_empty_theme_toml_still_follows_the_portal() {
        use alpheus_common::theme::presets::preset;
        let dir = temp("blank-portal");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(THEME_FILE), "").unwrap();
        let auto = ThemeSettings::default();
        let catppuccin = preset("catppuccin").unwrap();
        let r = reload_settings(&dir, &auto, || Some(Mode::Light));
        assert_eq!(r.resolved.roles, catppuccin.light.unwrap(), "light");
        let r = reload_settings(&dir, &auto, || Some(Mode::Dark));
        assert_eq!(r.resolved.roles, catppuccin.dark, "dark");
        let dark = ThemeSettings { mode: ModeSetting::Dark, ..auto };
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

    fn temp(name: &str) -> PathBuf {
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
        assert_eq!(scan(&buf, Some((On::Dir(3), b"alpheus")), None, &no_link), Batch { theme: true, ..Batch::default() });
        let unrelated: Vec<u8> = (0..20).flat_map(|_| record(3, libc::IN_CLOSE_WRITE, "launcher.toml")).collect();
        assert_eq!(scan(&unrelated, Some((On::Dir(3), b"alpheus")), None, &no_link), Batch::default());
        let other_watch = record(9, libc::IN_CLOSE_WRITE, "theme.toml");
        assert_eq!(scan(&other_watch, Some((On::Dir(3), b"alpheus")), None, &no_link), Batch::default(), "another watch's record");
        assert!(scan(&record(-1, libc::IN_Q_OVERFLOW, ""), Some((On::Dir(3), b"alpheus")), None, &no_link).theme, "records lost: re-read");
    }

    #[test]
    fn the_directory_appearing_or_going_rearms() {
        let p = On::Parent(2);
        let both = Batch { theme: true, rearm: true, ..Batch::default() };
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "alpheus"), Some((p, b"alpheus")), None, &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_MOVED_TO | libc::IN_ISDIR, "alpheus"), Some((p, b"alpheus")), None, &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "other"), Some((p, b"alpheus")), None, &no_link), Batch::default());
        assert_eq!(
            scan(&record(2, libc::IN_CREATE, "alpheus"), Some((p, b"alpheus")), None, &no_link),
            Batch::default(),
            "a file named alpheus"
        );
        for gone in [libc::IN_DELETE_SELF, libc::IN_MOVE_SELF, libc::IN_IGNORED] {
            assert_eq!(scan(&record(4, gone, ""), Some((On::Dir(4), b"alpheus")), None, &no_link), both, "{gone:#x}");
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
        let mut w = Watches::start(&d, None).unwrap();
        assert!(matches!(w.theme_on(), On::Dir(_)), "{:?}", w.theme_on());
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
        let mut w = Watches::start(&d, None).unwrap();
        assert!(matches!(w.theme_on(), On::Parent(_)), "{:?}", w.theme_on());
        std::fs::create_dir(parent.join("other")).unwrap();
        assert_eq!(w.drain(), Batch::default(), "another directory: nothing");
        std::fs::create_dir(&d).unwrap();
        assert_eq!(w.drain(), Batch { theme: true, rearm: true, ..Batch::default() }, "it appeared");
        assert!(matches!(w.theme_on(), On::Dir(_)), "{:?}", w.theme_on());
        std::fs::write(d.join("theme.toml.tmp"), "theme = \"nord\"\n").unwrap();
        std::fs::rename(d.join("theme.toml.tmp"), d.join(THEME_FILE)).unwrap();
        assert!(w.drain().theme, "renamed into place");
        std::fs::remove_file(d.join(THEME_FILE)).unwrap();
        std::fs::remove_dir(&d).unwrap();
        assert_eq!(w.drain(), Batch { theme: true, rearm: true, ..Batch::default() }, "it went");
        assert!(matches!(w.theme_on(), On::Parent(_)), "back to waiting in the parent: {:?}", w.theme_on());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn an_unwatchable_directory_is_one_line_never_a_panic() {
        let w = Watches::start(Path::new("/nonexistent-alpheus-parent/alpheus"), None).unwrap();
        assert!(!w.theme_armed(), "neither the directory nor its parent exists: no theme target, no panic");
        assert!(!w.shield_armed());
        assert!(
            arm(&Inotify::new().unwrap(), Path::new("/nonexistent-alpheus-parent/alpheus"), DIR_MASK, None)
                .unwrap_err()
                .text
                .starts_with("cannot watch /nonexistent-alpheus-parent")
        );
    }

    /// A directory created between the failed `add` and the parent's arm is found by
    /// the retry (its creation would otherwise never be reported by anyone).
    #[test]
    fn a_directory_created_while_the_parent_is_armed_is_found() {
        let parent = temp("arm-race");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = parent.join("late");
        let fd = Inotify::new().unwrap();
        let on = arm_with(&fd, &dir, DIR_MASK, None, || std::fs::create_dir(&dir).unwrap()).unwrap();
        assert!(matches!(on, On::Dir(_)), "the directory, not its parent: {on:?}");
        // Control: with nothing created in between, the parent waits.
        let other = parent.join("never");
        assert!(matches!(arm_with(&fd, &other, DIR_MASK, None, || {}).unwrap(), On::Parent(_)));
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// The parent's stray watch is dropped once the retry found the directory, unless the descriptor is
    /// another target's (inotify gives one per inode).
    #[test]
    fn the_strays_parent_watch_is_dropped_unless_another_target_owns_it() {
        let parent = temp("arm-stray");
        std::fs::create_dir_all(&parent).unwrap();
        let events = |fd: &Inotify| {
            let mut n = Vec::new();
            fd.drain(|c| n.extend(Events(c).map(|e| e.wd)));
            n
        };
        let fd = Inotify::new().unwrap();
        let dir = parent.join("late");
        arm_with(&fd, &dir, DIR_MASK, None, || std::fs::create_dir(&dir).unwrap()).unwrap();
        events(&fd);
        std::fs::write(parent.join("x"), b"").unwrap();
        assert!(events(&fd).is_empty(), "the stray parent watch is gone");
        // The same parent, watched by another target: kept.
        let fd = Inotify::new().unwrap();
        let owned = fd.add(&parent, DIR_MASK).unwrap();
        let dir2 = parent.join("late2");
        arm_with(&fd, &dir2, DIR_MASK, Some(owned), || std::fs::create_dir(&dir2).unwrap()).unwrap();
        events(&fd);
        std::fs::write(parent.join("y"), b"").unwrap();
        assert_eq!(events(&fd), vec![owned], "the other target's watch stays");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// A retry that fails for another reason than "missing" is an error (never a silent wait in the
    /// parent for a directory that cannot come), and leaves no stray watch.
    #[test]
    fn a_retry_failing_for_another_reason_is_an_error_not_a_wait() {
        let parent = temp("arm-retry-err");
        std::fs::create_dir_all(&parent).unwrap();
        let fd = Inotify::new().unwrap();
        let dir = parent.join("file");
        // A regular file where the directory should come: IN_ONLYDIR refuses it with ENOTDIR.
        let r = arm_with(&fd, &dir, DIR_MASK, None, || std::fs::write(&dir, b"").unwrap());
        assert!(r.is_err(), "{r:?}");
        fd.drain(|_| {});
        std::fs::write(parent.join("z"), b"").unwrap();
        let mut n = 0;
        fd.drain(|_| n += 1);
        assert_eq!(n, 0, "no stray watch left");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn the_shield_line_carries_a_reason_never_a_path() {
        let e = arm(&Inotify::new().unwrap(), Path::new("/nonexistent-alpheus-parent/state"), FLAG_DIR_MASK, None).unwrap_err();
        let line = shield_unwatched_line(&e);
        assert!(line.starts_with("shield: flag not watched ("), "{line}");
        assert!(!line.contains("nonexistent-alpheus-parent") && !line.contains('/'), "{line}");
        assert!(line.contains("not found"), "{line}");
        // The theme's own line keeps its path.
        assert!(e.text.contains("nonexistent-alpheus-parent"));
    }

    #[test]
    fn a_failed_restore_of_the_themes_mask_rearms_the_theme() {
        let cfg = temp("restore-cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let state = temp("restore-state");
        std::fs::create_dir_all(&state).unwrap();
        let mut w = Watches::start(&cfg, Some(&state.join("on"))).unwrap();
        // Force the shared-descriptor case by hand, then lose the theme's directory before the restore.
        let theme_on = w.theme_on();
        w.shield.as_mut().unwrap().target.on = theme_on;
        std::fs::remove_dir_all(&cfg).unwrap();
        w.settle();
        assert!(w.shield.is_none(), "the shield target gave way");
        assert!(matches!(w.theme_on(), On::Parent(_) | On::Nothing), "{:?}: re-armed, not left on a dead descriptor", w.theme_on());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn no_flag_is_watched_without_a_shield_app() {
        assert_eq!(flag_to_watch(None), None);
        let c = crate::config::ShieldConfig {
            flag: "/var/lib/test-shield/on".into(),
            on: vec!["x".into()],
            off: vec!["x".into()],
            ..Default::default()
        };
        let app = crate::shield::app::ShieldApp::new(&c).unwrap();
        assert_eq!(flag_to_watch(Some(&app)), Some(PathBuf::from("/var/lib/test-shield/on")));
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

    fn shield_scan<'a>(on: On) -> Option<ShieldScan<'a>> {
        Some((on, b"state", b"on"))
    }

    #[test]
    fn the_flag_records_count_only_for_its_name() {
        for mask in [libc::IN_CREATE, libc::IN_DELETE, libc::IN_MOVED_TO, libc::IN_MOVED_FROM] {
            let r = record(5, mask, "on");
            let b = scan(&r, None, shield_scan(On::Dir(5)), &no_link);
            assert_eq!(b, Batch { shield: true, ..Batch::default() }, "{mask:#x}");
        }
        for other in ["on.tmp", "other", "", "ON"] {
            let r = record(5, libc::IN_CREATE, other);
            assert_eq!(scan(&r, None, shield_scan(On::Dir(5)), &no_link), Batch::default(), "{other}");
        }
        let dir_named_on = record(5, libc::IN_CREATE | libc::IN_ISDIR, "on");
        assert_eq!(scan(&dir_named_on, None, shield_scan(On::Dir(5)), &no_link), Batch::default());
        let other_wd = record(6, libc::IN_CREATE, "on");
        assert_eq!(scan(&other_wd, None, shield_scan(On::Dir(5)), &no_link), Batch::default());
        // Its content never matters: a write to the flag is not a trigger.
        let written = record(5, libc::IN_CLOSE_WRITE | libc::IN_MODIFY, "on");
        assert_eq!(scan(&written, None, shield_scan(On::Dir(5)), &no_link), Batch::default());
    }

    #[test]
    fn the_flag_directory_appearing_or_going_rearms_the_shield_target_only() {
        let both = Batch { shield: true, shield_rearm: true, ..Batch::default() };
        let p = On::Parent(2);
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "state"), None, shield_scan(p), &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_MOVED_TO | libc::IN_ISDIR, "state"), None, shield_scan(p), &no_link), both);
        assert_eq!(scan(&record(2, libc::IN_CREATE | libc::IN_ISDIR, "other"), None, shield_scan(p), &no_link), Batch::default());
        for gone in [libc::IN_DELETE_SELF, libc::IN_MOVE_SELF, libc::IN_IGNORED] {
            assert_eq!(scan(&record(4, gone, ""), None, shield_scan(On::Dir(4)), &no_link), both, "{gone:#x}");
        }
        // With both targets armed, the theme's directory going leaves the shield alone, and back.
        let theme = Some((On::Dir(3), &b"alpheus"[..]));
        let b = scan(&record(3, libc::IN_DELETE_SELF, ""), theme, shield_scan(On::Dir(4)), &no_link);
        assert_eq!(b, Batch { theme: true, rearm: true, ..Batch::default() });
        let b = scan(&record(4, libc::IN_DELETE_SELF, ""), theme, shield_scan(On::Dir(4)), &no_link);
        assert_eq!(b, both);
    }

    #[test]
    fn an_overflow_rereads_both() {
        let r = record(-1, libc::IN_Q_OVERFLOW, "");
        let theme = Some((On::Dir(3), &b"alpheus"[..]));
        let b = scan(&r, theme, shield_scan(On::Dir(4)), &no_link);
        assert_eq!(b, Batch { theme: true, shield: true, ..Batch::default() });
        assert_eq!(scan(&r, None, None, &no_link), Batch::default(), "no target, nothing to re-read");
    }

    #[test]
    fn a_failed_theme_target_leaves_the_shield_target_and_the_converse() {
        let state = temp("both-state");
        std::fs::create_dir_all(&state).unwrap();
        let w = Watches::start(Path::new("/nonexistent-alpheus-parent/alpheus"), Some(&state.join("on"))).unwrap();
        assert!(!w.theme_armed() && w.shield_armed(), "the theme target failed, the flag's armed");
        let cfg = temp("both-cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let w = Watches::start(&cfg, Some(Path::new("/nonexistent-alpheus-parent/state/on"))).unwrap();
        assert!(w.theme_armed() && !w.shield_armed(), "the flag's directory cannot be watched: the theme's armed");
        // And a working pair: the theme still sees its file, the shield the flag.
        let mut w = Watches::start(&cfg, Some(&state.join("on"))).unwrap();
        assert!(w.theme_armed() && w.shield_armed());
        std::fs::write(cfg.join(THEME_FILE), "x\n").unwrap();
        assert_eq!(w.drain(), Batch { theme: true, ..Batch::default() });
        std::fs::write(state.join("on"), "").unwrap();
        assert_eq!(w.drain(), Batch { shield: true, ..Batch::default() });
        std::fs::remove_file(state.join("on")).unwrap();
        assert_eq!(w.drain(), Batch { shield: true, ..Batch::default() });
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn a_shared_descriptor_drops_the_shield_target() {
        let cfg = temp("shared-cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let alias = temp("shared-alias");
        std::os::unix::fs::symlink(&cfg, &alias).unwrap();
        let mut w = Watches::start(&cfg, Some(&alias.join("on"))).unwrap();
        assert!(w.theme_armed());
        assert!(!w.shield_armed(), "one inode, one descriptor: the shield target gives way");
        // The theme's own mask came back: a close-write of a theme file is still seen.
        std::fs::write(cfg.join(THEME_FILE), "x\n").unwrap();
        assert_eq!(w.drain(), Batch { theme: true, ..Batch::default() });
        let _ = std::fs::remove_file(&alias);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn a_missing_flag_directory_is_awaited_in_its_parent() {
        let cfg = temp("missing-cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let parent = temp("missing-parent");
        std::fs::create_dir_all(&parent).unwrap();
        let state = parent.join("state");
        let flag = state.join("on");
        let mut w = Watches::start(&cfg, Some(&flag)).unwrap();
        assert!(matches!(w.shield_on(), On::Parent(_)), "{:?}", w.shield_on());
        std::fs::create_dir(parent.join("other")).unwrap();
        assert_eq!(w.drain(), Batch::default(), "another directory: nothing");
        std::fs::create_dir(&state).unwrap();
        assert_eq!(w.drain(), Batch { shield: true, shield_rearm: true, ..Batch::default() }, "it appeared");
        assert!(matches!(w.shield_on(), On::Dir(_)), "{:?}", w.shield_on());
        std::fs::write(&flag, "").unwrap();
        assert_eq!(w.drain(), Batch { shield: true, ..Batch::default() }, "the flag created");
        std::fs::rename(&flag, state.join("gone")).unwrap();
        assert_eq!(w.drain(), Batch { shield: true, ..Batch::default() }, "the flag renamed away");
        std::fs::remove_file(state.join("gone")).unwrap();
        std::fs::remove_dir(&state).unwrap();
        assert_eq!(w.drain(), Batch { shield: true, shield_rearm: true, ..Batch::default() }, "it went");
        assert!(matches!(w.shield_on(), On::Parent(_)), "back to waiting: {:?}", w.shield_on());
        let _ = std::fs::remove_dir_all(&parent);
        let _ = std::fs::remove_dir_all(&cfg);
    }
}
