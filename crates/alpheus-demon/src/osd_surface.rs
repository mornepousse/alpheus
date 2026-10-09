//! The OSD's Wayland side (OSD spec § 3), kept thin. One `Overlay` layer surface on the focused
//! output (the compositor's choice when it is unknown), anchored at the bottom only: centred,
//! `margin` px above the edge, exclusive zone 0. No keyboard, and an EMPTY input region: it never
//! takes a click. Created on show, dropped on hide with its own small pool, so nothing of it stays
//! resident. One `DeadlineTimer` (`App::osd_timer`) hides it `timeout_ms` after the last change.
//! Overlay, because Hyprland draws Top under a fullscreen window.

use crate::backlight;
use crate::bubbles;
use crate::config::ModuleId;
use crate::launcher::surface::{Redraw, plan_configure, plan_redraw};
use crate::list::Boxed;
use crate::log::write as log;
use crate::osd::{self, Heard, Pill};
use crate::painter::FrameInk;
use crate::popup::box_size;
use crate::system::SoundState;
use crate::wayland::{App, pick_buffer};
use alpheus_commun::launcher_ipc::{OsdVerb, Reply, Request};
use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use std::time::Instant;

const NAMESPACE: &str = "alpheus-osd";

/// Everything the shown pill holds; dropped whole when it hides.
pub struct OpenOsd {
    layer: LayerSurface,
    /// Its own pool: dropped with the pill (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// The row shown, and the logical size asked or imposed.
    boxed: Boxed,
    size: (u32, u32),
    /// Some: the focused output, found by name; None: the compositor's choice.
    output: Option<wl_output::WlOutput>,
    output_name: Option<String>,
    scale: i32,
    configured: bool,
    /// The row last committed: an unchanged one is not repainted.
    last: Option<Boxed>,
    /// Changes shown on this surface (a held key: many), logged when it goes.
    shown: u32,
}

impl App {
    fn osd_enabled(&self) -> bool {
        self.bar.config.modules.contains(&ModuleId::Osd)
    }

    /// Every sound message, before the bar applies it (`launch::run`'s mail callback): the trigger
    /// rules (`osd::Watch`), then the pill when they say so. Nothing without the module.
    pub(crate) fn osd_sound(&mut self, s: Option<&SoundState>) {
        if !self.osd_enabled() {
            return;
        }
        let now = Instant::now();
        match self.osd.watch.sound(s, now) {
            Heard::Baseline => log("osd: sound baseline"),
            Heard::Show(pill) => self.osd_show(pill, now),
            Heard::Quiet => {}
        }
    }

    /// The one way to show something (sound and the brightness verb): the pill replaced, its
    /// deadline `timeout_ms` from now, the surface created or updated, the timer re-armed.
    pub(crate) fn osd_show(&mut self, pill: Pill, now: Instant) {
        let timeout = osd::timeout(&self.bar.config.osd);
        let what = osd::describe(&pill);
        self.osd.show(pill, now, timeout);
        self.osd_sync(&what);
    }

    /// `alpheus-launch --osd brightness` (spec § 2, § 4): the backlight read once, then the pill.
    /// Always Done: with the module off or no backlight, nothing shows (exit 0).
    pub(crate) fn osd_verb(&mut self, request: &Request) -> Reply {
        if !matches!(request, Request::Osd(OsdVerb::Brightness)) {
            return Reply::Done;
        }
        if !self.osd_enabled() {
            log("osd: brightness asked, but the osd module is not in modules");
            return Reply::Done;
        }
        let cfg = &self.bar.config.osd;
        match backlight::level(&cfg.backlight_dir, &cfg.backlight) {
            Some(p) => self.osd_show(Pill::Brightness(p), Instant::now()),
            None => log("osd: no backlight device, nothing shown"),
        }
        Reply::Done
    }

    /// The surface follows the model: none when nothing shows; created on the focused output, or
    /// updated in place (moved only when its content changed and the focus moved); then the timer.
    fn osd_sync(&mut self, what: &str) {
        match self.osd.shown().map(|p| osd::rows(p, self.bar.config.osd.width)) {
            None => self.osd_drop("hidden"),
            Some(boxed) => {
                let changed = self.osd_open.as_ref().is_some_and(|o| o.boxed != boxed);
                let on = self.osd_open.as_ref().and_then(|o| o.output_name.clone());
                let (_, focused) = self.bubbles_target();
                if self.osd_open.is_some() && bubbles::moves(changed, focused.as_deref(), on.as_deref()) {
                    self.osd_drop("focus moved");
                }
                if self.osd_open.is_some() {
                    self.osd_update(boxed);
                } else {
                    self.osd_create(boxed, what);
                }
            }
        }
        self.osd_timer_sync();
    }

    /// The pill's logical size at `scale`: its cells plus the frame's room (`popup::box_size`).
    fn osd_size(&mut self, boxed: &Boxed, scale: i32) -> (u32, u32) {
        let e = scale.max(1);
        let (columns, rows) = boxed.cells();
        let row_height = self.bar.config.height * e as u32;
        let advance = self.fonts.advance(e);
        let frame = self.fonts.frame(e);
        let (w, h) = box_size(columns, rows, advance, row_height, e, &frame);
        (w.max(1), h.max(1))
    }

    fn osd_create(&mut self, boxed: Boxed, what: &str) {
        // Made first: without an (empty) input region the pill would take every click, so it is
        // not shown at all.
        let region = match Region::new(&self.compositor) {
            Ok(r) => r,
            Err(e) => {
                log(&format!("osd: input region: {e}, not shown"));
                return;
            }
        };
        let (output, output_name) = self.bubbles_target();
        let scale = bubbles::start_scale(output.as_ref().and_then(|o| self.outputs.info(o)).map(|i| i.scale_factor));
        let size = self.osd_size(&boxed, scale);
        let k = scale as usize;
        let bytes = (size.0 as usize * k) * (size.1 as usize * k) * 4;
        let pool = match SlotPool::new((2 * bytes).max(4096), &self.shm) {
            Ok(p) => p,
            Err(e) => {
                log(&format!("osd: no shared buffer ({e}), not shown"));
                return;
            }
        };
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layers.create_layer_surface(&self.qh, surface, Layer::Overlay, Some(NAMESPACE), output.as_ref());
        // Bottom only: no left or right anchor centres it. Zone 0: windows do not move for it.
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_margin(0, 0, i32::try_from(self.bar.config.osd.margin).unwrap_or(0), 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(size.0, size.1);
        // Empty: copied by the request, so the region may be dropped right after.
        layer.set_input_region(Some(region.wl_region()));
        layer.commit();
        let place = output_name.as_deref().unwrap_or("the compositor's choice of output");
        log(&format!("osd surface on {place}: {what}"));
        self.osd_open = Some(OpenOsd {
            layer,
            pool,
            buffers: Vec::new(),
            boxed,
            size,
            output,
            output_name,
            scale,
            configured: false,
            last: None,
            shown: 1,
        });
    }

    /// A change on the open pill: counted; repainted only when its row differs.
    fn osd_update(&mut self, boxed: Boxed) {
        let Some(open) = self.osd_open.as_mut() else { return };
        open.shown = open.shown.saturating_add(1);
        if open.boxed == boxed {
            return;
        }
        let scale = open.scale;
        open.boxed = boxed.clone();
        let size = self.osd_size(&boxed, scale);
        self.osd_resize(size);
    }

    /// A new size is asked (its configure draws), the same one is drawn now, nothing before the
    /// first configure.
    fn osd_resize(&mut self, wanted: (u32, u32)) {
        let Some(open) = self.osd_open.as_mut() else { return };
        match plan_redraw(open.size, wanted, open.configured) {
            Redraw::Resize(w, h) => {
                open.size = (w, h);
                open.layer.set_size(w, h);
                open.layer.commit();
                open.last = None;
            }
            Redraw::Draw => self.osd_draw(),
            Redraw::Wait => open.layer.commit(),
        }
    }

    /// Paints the row as a boxed surface (the popups' pixel frame, on `popup_bg`). An unchanged row
    /// commits nothing.
    fn osd_draw(&mut self) {
        let height = self.bar.config.height;
        let App { osd_open, fonts, roles, .. } = self;
        let Some(open) = osd_open.as_mut() else { return };
        if !open.configured || open.last.as_ref() == Some(&open.boxed) {
            return;
        }
        let e = open.scale.max(1);
        let ue = e as u32;
        let (w, h) = (open.size.0 * ue, open.size.1 * ue);
        let OpenOsd { buffers, pool, layer, boxed, .. } = open;
        let Some(used) = pick_buffer(buffers, pool, w, h) else { return };
        let Some(px) = buffers[used].0.canvas(pool) else {
            log(&format!("osd buffer {w}×{h}: unexpectedly busy"));
            return;
        };
        px.fill(0);
        fonts.paint_boxed(boxed, roles, px, w, h, height * ue, e, FrameInk::Border);
        let surface = layer.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        layer.commit();
        open.last = Some(open.boxed.clone());
    }

    fn osd_drop(&mut self, why: &str) {
        let Some(open) = self.osd_open.take() else { return };
        let shown = open.shown;
        drop(open);
        log(&format!("osd surface dropped ({why}, {shown} shown)"));
        // The pool and its buffers: give the pages back.
        crate::memory::release_memory();
    }

    /// Whether `surface` is the pill's (checked by identity, before the bar's own handling).
    pub(crate) fn osd_owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.osd_open.as_ref().is_some_and(|o| o.layer.wl_surface() == surface)
    }

    /// The pill's configure (the caller checked `osd_owns`): an imposed size is obeyed, then drawn.
    pub(crate) fn osd_configured(&mut self, size: (u32, u32)) {
        let Some(open) = self.osd_open.as_mut() else { return };
        if let Some(imposed) = plan_configure(open.size, size) {
            open.size = imposed;
        }
        open.configured = true;
        open.last = None;
        self.osd_draw();
    }

    /// The compositor closed the pill (its output going, or its own reasons): hidden, never
    /// reopened; the next change shows a new one.
    pub(crate) fn osd_layer_closed(&mut self) {
        self.osd.hide();
        self.osd_drop("closed by the compositor");
        self.osd_timer_sync();
    }

    /// An output went away: the pill on it is hidden and dropped (it is transient: never moved).
    pub(crate) fn osd_output_gone(&mut self, output: &wl_output::WlOutput) {
        if !self.osd_open.as_ref().is_some_and(|o| bubbles::holds(o.output.as_ref(), output)) {
            return;
        }
        self.osd.hide();
        self.osd_drop("output gone");
        self.osd_timer_sync();
    }

    /// A scale change of the pill's surface: sized and drawn again at the new scale; false: not ours.
    pub(crate) fn osd_scale(&mut self, surface: &wl_surface::WlSurface, factor: i32) -> bool {
        if !self.osd_owns(surface) {
            return false;
        }
        let Some((boxed, scale)) = self.osd_open.as_ref().map(|o| (o.boxed.clone(), o.scale)) else { return true };
        if scale == factor {
            return true;
        }
        let size = self.osd_size(&boxed, factor);
        if let Some(open) = self.osd_open.as_mut() {
            open.scale = factor;
            open.last = None;
        }
        self.osd_resize(size);
        true
    }

    /// A restyle or a theme change (spec § 3): the shown pill painted again with the current roles.
    pub(crate) fn osd_repaint(&mut self) {
        if let Some(open) = self.osd_open.as_mut() {
            open.last = None;
        }
        self.osd_draw();
    }

    /// The hide timer follows the pill's deadline: armed only while it shows (spec § 3).
    pub(crate) fn osd_timer_sync(&mut self) {
        let deadline = self.osd.deadline();
        if let Some(handle) = self.loop_handle.clone() {
            self.osd_timer.sync(&handle, deadline, App::osd_expire);
        }
    }

    /// The timer fired (calloop callback; its source is dropped). Expired at the real time, never
    /// before the deadline it was armed for; a later change moved the deadline: re-armed.
    pub(crate) fn osd_expire(app: &mut App, deadline: Instant) {
        app.osd_timer.fired();
        if app.osd.expire(Instant::now().max(deadline)) {
            app.osd_drop("timeout");
        }
        app.osd_timer_sync();
    }
}
