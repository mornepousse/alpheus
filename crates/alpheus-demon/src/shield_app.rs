//! Shield mode on the `App` (shield spec): the model, the start and the flag's re-reads. Nothing
//! exists without a configured `[shield]` (every call site is one `Option` check).

use crate::config::ShieldConfig;
use crate::deadline::DeadlineTimer;
use crate::log::write as log;
use crate::shield::{Edge, Finish, Job, NoticeKind, Reason, Shield, ShieldMail, read_flag};
use crate::shield_run::spawn_job;
use crate::shield_usb::{Parsed, allow_argv};
use crate::startup::Mail;
use crate::wayland::App;
use std::path::Path;
use std::time::Instant;

/// Who asked for a read of the blocked devices; they join the one read in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // built by the detail (task 5)
pub(crate) enum ListFor {
    Detail,
}

pub(crate) struct ShieldApp {
    pub(crate) model: Shield,
    pub(crate) config: ShieldConfig,
    /// Armed only while a switch waits for its flag: the 5 s grace or the 60 s bound.
    timer: DeadlineTimer,
    /// Who waits for the read in flight.
    askers: Vec<ListFor>,
    /// The last read of the blocked devices (the detail, the picker and `status` take it).
    #[allow(dead_code)] // read by the detail and the verbs (tasks 5 and 6)
    pub(crate) listing: Option<Result<Parsed, Reason>>,
}

impl ShieldApp {
    /// None unless `[shield]` is configured.
    pub(crate) fn new(config: &ShieldConfig) -> Option<ShieldApp> {
        config.configured().then(|| ShieldApp {
            model: Shield::default(),
            config: config.clone(),
            timer: DeadlineTimer::new(),
            askers: Vec::new(),
            listing: None,
        })
    }

    pub(crate) fn flag(&self) -> &Path {
        Path::new(&self.config.flag)
    }
}

/// A notice as the server takes it: app `alpheus`, normal urgency, the server's default timeout.
pub(crate) fn incoming(kind: &NoticeKind, config: &ShieldConfig) -> crate::notify::Incoming {
    let (summary, body) = crate::shield::notice(kind, config);
    crate::notify::Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary,
        body,
        actions: Vec::new(),
        urgency: crate::notify::Urgency::Normal,
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: None,
    }
}

impl App {
    /// The flag's directory is watched, now: derived from the live watch, so a rearm that failed or a
    /// descriptor shared with the theme's (the target dropped) makes the degraded re-reads happen.
    /// Without a watch the flag is re-read after each command alpheus runs, at each panel open and at
    /// a switch's deadline, never on a timer.
    pub(crate) fn shield_watched(&self) -> bool {
        self.theme.watch.as_ref().is_some_and(crate::watch::Watches::shield_armed)
    }

    /// Reads the flag once and seeds the model (no notice, no hook), once the watch is armed.
    pub(crate) fn shield_start(&mut self) {
        let Some(sh) = self.shield.as_mut() else { return };
        let now = read_flag(sh.flag());
        sh.model.observe(now);
        if sh.model.first_unknown() {
            log("shield: the flag cannot be read");
        }
        self.shield_publish();
    }

    /// The flag's directory reported a change (or the queue overflowed), or a re-read is due: one
    /// reading, an edge logged and announced, the attempt it ends settled, the view refreshed.
    pub(crate) fn shield_readable(&mut self) -> Option<Edge> {
        let sh = self.shield.as_mut()?;
        let now = read_flag(sh.flag());
        let (edge, finish) = sh.model.flag(now, Instant::now());
        if sh.model.first_unknown() {
            log("shield: the flag cannot be read");
        }
        match edge {
            Some(Edge::On) => log("shield: now on"),
            Some(Edge::Off) => log("shield: now off"),
            None => {}
        }
        if let Some(f) = finish {
            self.shield_finish(f);
        }
        match edge {
            Some(Edge::On) => self.shield_notice(NoticeKind::On),
            Some(Edge::Off) => self.shield_notice(NoticeKind::Off),
            None => {}
        }
        self.shield_publish();
        self.shield_timer_sync();
        edge
    }

    /// Starts the switch to `target` (the other state than the one known): Err says why not.
    #[allow(dead_code)] // the row and the verbs (tasks 5 and 6)
    pub(crate) fn shield_switch(&mut self, target: bool) -> Result<(), &'static str> {
        let sh = self.shield.as_mut().ok_or("shield is not configured in bar.toml")?;
        match sh.model.state() {
            None => return Err("the shield state is unknown"),
            Some(on) if on == target => return Err("already in that state"),
            Some(_) => {}
        }
        let (target, generation) = sh.model.begin_switch(Instant::now()).ok_or("a shield command is already running")?;
        let argv = if target { sh.config.on.clone() } else { sh.config.off.clone() };
        log(if target { "shield: on" } else { "shield: off" });
        self.shield_spawn(Job::Switch(target), generation, argv);
        self.shield_publish();
        self.shield_timer_sync();
        Ok(())
    }

    /// Allows the blocked device `id`: Err says why not.
    #[allow(dead_code)] // the detail and the picker (tasks 5 and 6)
    pub(crate) fn shield_allow(&mut self, id: u32) -> Result<(), &'static str> {
        let sh = self.shield.as_mut().ok_or("shield is not configured in bar.toml")?;
        let argv = allow_argv(&sh.config.allow, id).ok_or("allow is not configured")?;
        let name = sh.listing.as_ref().and_then(|l| l.as_ref().ok()).and_then(|p| p.devices.iter().find(|d| d.id == id));
        let name = name.map_or_else(|| "?".to_string(), |d| d.name.clone());
        let generation = sh.model.begin_allow(id, name, Instant::now()).ok_or("a shield command is already running")?;
        log("shield: allow");
        self.shield_spawn(Job::Allow(id), generation, argv);
        self.shield_publish();
        Ok(())
    }

    /// Reads the blocked devices; an asker while one read runs joins it.
    #[allow(dead_code)] // the detail (task 5)
    pub(crate) fn shield_list(&mut self, why: ListFor) {
        let Some(sh) = self.shield.as_mut() else { return };
        let argv = sh.config.blocked_argv().to_vec();
        if argv.is_empty() {
            return;
        }
        if !sh.askers.contains(&why) {
            sh.askers.push(why);
        }
        if sh.model.begin_list() {
            self.shield_spawn(Job::List, 0, argv);
        }
    }

    /// One worker for one job; its report comes back as `Mail::Shield`. A thread that cannot start
    /// reports itself failed, so no slot stays held.
    fn shield_spawn(&mut self, job: Job, generation: u64, argv: Vec<String>) {
        let started = self.mail.clone().and_then(|tx| {
            spawn_job(self.exec.clone(), job, generation, argv, move |m| {
                let _ = tx.send(Mail::Shield(m));
            })
            .ok()
        });
        // The handle is dropped: the worker ends on its own once it has reported.
        if started.is_none() {
            self.shield_mail(ShieldMail::worker_failed(job, generation));
        }
    }

    /// A worker's report (the loop's mail): its slot freed, the live attempt's result handled, and in
    /// the degraded mode (no watch) the flag read again after a command.
    pub(crate) fn shield_mail(&mut self, m: ShieldMail) {
        let watched = self.shield_watched();
        let Some(sh) = self.shield.as_mut() else { return };
        let ShieldMail::Done { job, .. } = &m;
        let acted = *job != Job::List;
        let finish = sh.model.done(m, Instant::now());
        let degraded = acted && !watched;
        if let Some(f) = finish {
            self.shield_finish(f);
        }
        if degraded {
            self.shield_readable();
        }
        self.shield_publish();
        self.shield_timer_sync();
    }

    /// The timer fired: calloop dropped it, then the model decides.
    fn shield_tick(app: &mut App, now: Instant) {
        let watched = app.shield_watched();
        let Some(sh) = app.shield.as_mut() else { return };
        sh.timer.fired();
        if !watched {
            // No watch: the deadline is the one moment the flag is read again (once, never polled), so a
            // change nobody told us about ends the attempt as a success, not as "unchanged".
            app.shield_readable();
        }
        let Some(sh) = app.shield.as_mut() else { return };
        if let Some(f) = sh.model.expired(now) {
            app.shield_finish(f);
        }
        app.shield_publish();
        app.shield_timer_sync();
    }

    /// The timer follows the model: armed while a switch waits, removed otherwise.
    fn shield_timer_sync(&mut self) {
        let Some(sh) = self.shield.as_mut() else { return };
        sh.timer.sync_opt(self.loop_handle.as_ref(), sh.model.deadline(), App::shield_tick);
    }

    /// What a command's or a read's end means: a line from the closed list, a notice, the listing kept.
    fn shield_finish(&mut self, f: Finish) {
        let Some(sh) = self.shield.as_mut() else { return };
        match f {
            // The flag's edge logs and announces it (whoever made the change).
            Finish::Switched(_) => {}
            Finish::SwitchFailed(target, why) => {
                log(&format!("shield: switching {} failed ({})", if target { "on" } else { "off" }, why.text()));
                self.shield_notice(NoticeKind::SwitchFailed);
            }
            Finish::Allowed(_, name) => {
                self.shield_notice(NoticeKind::Allowed(name));
            }
            Finish::AllowFailed(_, why) => {
                log(&format!("shield: allow failed ({})", why.text()));
                self.shield_notice(NoticeKind::AllowFailed);
            }
            Finish::Listed(p) => {
                if p.more {
                    log("shield: more than 64 blocked devices; the rest not listed");
                }
                sh.listing = Some(Ok(p));
                sh.askers.clear();
            }
            Finish::ListFailed(why) => {
                log(&format!("shield: listing failed ({})", why.text()));
                sh.listing = Some(Err(why));
                sh.askers.clear();
            }
        }
    }

    /// `Bar.shield` follows the model; a change marks the bar dirty.
    pub(crate) fn shield_publish(&mut self) {
        let Some(sh) = self.shield.as_ref() else { return };
        let view = sh.model.view(&sh.config, Instant::now());
        if self.bar.shield != Some(view) {
            self.bar.shield = Some(view);
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests;
