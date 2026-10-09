//! The notification bubbles' Wayland side (notifications spec § 3), kept thin. One `Overlay` layer
//! surface on the focused output (the compositor's choice when it is unknown), anchored at the top
//! only: centred, under the bar's exclusive zone (zone 0), sized to the stack. No keyboard; an input
//! region per bubble; a pool of its own. Everything lives in `OpenBubbles`, created with the first
//! bubble and dropped whole with the last: nothing of it stays resident. Overlay, because Hyprland
//! draws Top under a fullscreen window.

use crate::actions::{self, Button};
use crate::bubbles::{self, Activation, BubbleBox, DEFAULT_ACTION};
use crate::launcher::surface::{Redraw, plan_configure, plan_redraw};
use crate::log::write as log;
use crate::notify::CloseReason;
use crate::painter::FrameInk;
use crate::popup::box_size;
use crate::state::Feed;
use crate::tray::notifications::Notifications;
use crate::wayland::{App, notifications_of, pick_buffer};
use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use std::time::Instant;

const NAMESPACE: &str = "alpheus-bubbles";

/// Everything the shown bubbles hold; dropped whole with the last one.
pub struct OpenBubbles {
    layer: LayerSurface,
    /// Its own pool: dropped with the last bubble (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// What is shown, top to bottom (newest first), and each bubble's (top, height), logical px.
    stack: Vec<BubbleBox>,
    spans: Vec<(u32, u32)>,
    /// Logical size asked, or imposed by a configure.
    size: (u32, u32),
    /// Some: the focused output, found by name; None: the compositor's choice.
    output: Option<wl_output::WlOutput>,
    output_name: Option<String>,
    scale: i32,
    configured: bool,
    /// The stack last committed: an unchanged one is not repainted.
    last: Option<Vec<BubbleBox>>,
}

/// The stack's (id, revision) pairs: what a compositor `closed` is remembered by.
fn keys(stack: &[BubbleBox]) -> Vec<(u32, u32)> {
    stack.iter().map(|b| (b.id, b.revision)).collect()
}

/// What the compositor closing the bubbles' surface leaves behind: it is opened again once per
/// stack; closed again for that same stack, it stays closed until the stack changes. Never a
/// create/close loop.
/// A close followed, in the same dispatch batch, by an output going is that output's loss (an
/// unplug), not the compositor's choice: it reopens without spending the stack's one reopen.
#[derive(Debug, Default)]
pub(crate) struct Closes {
    reopened: Option<Vec<(u32, u32)>>,
    held: Option<Vec<(u32, u32)>>,
    /// A close waiting for the batch's end: the stack it showed, and whether an output went since.
    pending: Option<(Vec<(u32, u32)>, bool)>,
}

impl Closes {
    /// The compositor closed the surface while `keys` showed: decided by `settle` once the batch is
    /// over (an output may go in it).
    pub(crate) fn closed(&mut self, keys: Vec<(u32, u32)>) {
        self.pending = Some((keys, false));
    }

    /// An output went in this batch: a pending close was its loss.
    pub(crate) fn output_lost(&mut self) {
        if let Some((_, lost)) = self.pending.as_mut() {
            *lost = true;
        }
    }

    /// The pending close decided: Some(true), open it again (an output's loss, or the first close
    /// for this stack); Some(false), leave it closed; None, no close pending.
    pub(crate) fn settle(&mut self) -> Option<bool> {
        let (keys, lost) = self.pending.take()?;
        if lost {
            return Some(true);
        }
        if self.reopened.as_ref() == Some(&keys) {
            self.held = Some(keys);
            Some(false)
        } else {
            self.reopened = Some(keys);
            Some(true)
        }
    }

    /// Whether a surface may be created for `keys`: never for the stack the compositor closed twice.
    pub(crate) fn may_open(&mut self, keys: &[(u32, u32)]) -> bool {
        if self.held.as_deref() == Some(keys) {
            return false;
        }
        self.held = None;
        true
    }
}

impl App {
    /// The one path for a change made on the App's side (a bubble clicked or expired, do-not-disturb,
    /// Clear all; the panel, task 7): `notify_do` (signals sent, the view and bubbles deferred to the
    /// host's next output), then the bubbles and the open panel follow.
    pub(crate) fn notify_act(&mut self, f: impl FnOnce(&mut Notifications)) {
        self.notify_do(f);
        self.bubbles_sync();
        self.panel_refresh();
    }

    /// A left click on bubble (`id`, `revision`), or Enter on a live entry in the panel: the `default`
    /// action (`Center::invoke` emits `Invoked`, then closes a non-resident notification itself,
    /// `Closed(id, 2)`); without one, dismissed. A late click on a replaced or closed notification
    /// does nothing (spec § 3).
    pub(crate) fn notify_activate(&mut self, id: u32, revision: u32) {
        let now = Instant::now();
        self.notify_act(|n| {
            let what = bubbles::activation(bubbles::find(n.center(), id), revision);
            n.act(|c| match what {
                Activation::Invoke { .. } => c.invoke(id, revision, DEFAULT_ACTION, now),
                Activation::Dismiss => c.close(id, CloseReason::Dismissed, now),
                Activation::Stale => Vec::new(),
            });
        });
    }

    /// A right click on a bubble: dismissed (reason 2), nothing run.
    pub(crate) fn notify_dismiss(&mut self, id: u32) {
        let now = Instant::now();
        self.notify_act(|n| n.act(|c| c.close(id, CloseReason::Dismissed, now)));
    }

    /// The stack as the consumer has it now: empty without the module, while the name is not held
    /// (mako has it, or it was lost), or with nothing to show.
    fn bubble_stack(&self) -> Vec<BubbleBox> {
        let width = self.bar.config.notifications.width;
        notifications_of(self.tray.as_ref(), self.link.as_ref())
            .filter(|n| n.held())
            .map(|n| bubbles::stack(&n.center().bubbles(), width, crate::tray::notifications::minute_now()))
            .unwrap_or_default()
    }

    /// The focused output, by name among the bar's outputs, and that name; (None, None): unknown, the
    /// compositor chooses (as the launcher's box does).
    fn bubbles_target(&self) -> (Option<wl_output::WlOutput>, Option<String>) {
        let wanted = match &self.bar.compositor {
            Feed::Ready(c) => c.focused_output().map(str::to_owned),
            _ => None,
        };
        let named = wanted
            .as_deref()
            .and_then(|n| self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(n)));
        let name = named.as_ref().and(wanted);
        (named, name)
    }

    /// Each bubble's span and the surface's logical size at `scale`: every bubble as wide as the
    /// widest (they all hold `width` cells), the frame's room included (`popup::box_size`).
    fn bubbles_layout(&mut self, stack: &[BubbleBox], scale: i32) -> (Vec<(u32, u32)>, (u32, u32)) {
        let e = scale.max(1);
        let row_height = self.bar.config.height * e as u32;
        let advance = self.fonts.advance(e);
        let frame = self.fonts.frame(e);
        let sizes: Vec<(u32, u32)> = stack
            .iter()
            .map(|b| {
                let (columns, rows) = b.boxed.cells();
                box_size(columns, rows, advance, row_height, e, &frame)
            })
            .collect();
        let width = sizes.iter().map(|s| s.0).max().unwrap_or(0);
        let heights: Vec<u32> = sizes.iter().map(|s| s.1).collect();
        let spans = bubbles::spans(&heights, bubbles::GAP);
        let size = (width.max(1), bubbles::total_height(&spans).max(1));
        (spans, size)
    }

    /// After anything that may change the bubbles (a host's output flagged them, a click, an expiry,
    /// the panel): the stack rebuilt; the surface created with the first bubble, redrawn when the stack
    /// changed (moved to the focused output if that changed meanwhile), dropped with the last; then the
    /// expiry timer follows.
    pub(crate) fn bubbles_sync(&mut self) {
        let stack = self.bubble_stack();
        if stack.is_empty() {
            self.bubbles_drop("empty");
        } else {
            let changed = self.bubbles.as_ref().is_some_and(|o| o.stack != stack);
            let on = self.bubbles.as_ref().and_then(|o| o.output_name.clone());
            let (_, focused) = self.bubbles_target();
            if self.bubbles.is_some() && bubbles::moves(changed, focused.as_deref(), on.as_deref()) {
                self.bubbles_drop("focus moved");
            }
            if self.bubbles.is_some() {
                self.bubbles_update(stack);
            } else if self.bubble_closes.may_open(&keys(&stack)) {
                self.bubbles_create(stack);
            }
        }
        self.bubble_timer_sync();
    }

    fn bubbles_create(&mut self, stack: Vec<BubbleBox>) {
        let (output, output_name) = self.bubbles_target();
        let scale = bubbles::start_scale(output.as_ref().and_then(|o| self.outputs.info(o)).map(|i| i.scale_factor));
        let (spans, size) = self.bubbles_layout(&stack, scale);
        let k = scale as usize;
        let bytes = (size.0 as usize * k) * (size.1 as usize * k) * 4;
        let pool = match SlotPool::new((2 * bytes).max(4096), &self.shm) {
            Ok(p) => p,
            Err(e) => {
                log(&format!("bubbles: no shared buffer ({e}), not shown"));
                return;
            }
        };
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layers.create_layer_surface(&self.qh, surface, Layer::Overlay, Some(NAMESPACE), output.as_ref());
        // Top only: no left or right anchor centres it; zone 0 keeps it below the bar's zone.
        layer.set_anchor(Anchor::TOP);
        layer.set_margin(bubbles::GAP as i32, 0, 0, 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(size.0, size.1);
        self.bubbles_input(&layer, &spans, size.0);
        layer.commit();
        let place = output_name.as_deref().unwrap_or("the compositor's choice of output");
        log(&format!("bubbles surface on {place}"));
        self.bubbles = Some(OpenBubbles {
            layer,
            pool,
            buffers: Vec::new(),
            stack,
            spans,
            size,
            output,
            output_name,
            scale,
            configured: false,
            last: None,
        });
    }

    /// One input rect per bubble, uncommitted (the caller's commit applies it): a click in a gap, or
    /// beside the bubbles, reaches what is under them.
    fn bubbles_input(&self, layer: &LayerSurface, spans: &[(u32, u32)], width: u32) {
        match Region::new(&self.compositor) {
            Ok(region) => {
                for (x, y, w, h) in bubbles::input_rects(spans, width) {
                    region.add(x, y, w, h);
                }
                // Copied by the request: the region may be destroyed (dropped) right after.
                layer.set_input_region(Some(region.wl_region()));
            }
            Err(e) => log(&format!("bubbles: input region: {e}")),
        }
    }

    /// A changed stack on the open surface: nothing when it did not change; otherwise new spans,
    /// then the size.
    fn bubbles_update(&mut self, stack: Vec<BubbleBox>) {
        let Some(scale) = self.bubbles.as_ref().filter(|o| o.stack != stack).map(|o| o.scale) else { return };
        let (spans, size) = self.bubbles_layout(&stack, scale);
        let Some(open) = self.bubbles.as_mut() else { return };
        open.stack = stack;
        open.spans = spans;
        self.bubbles_resize(size);
    }

    /// The input region re-sent, then the size: a new one is asked (its configure draws), the same one
    /// is drawn now, nothing before the first configure.
    fn bubbles_resize(&mut self, wanted: (u32, u32)) {
        let Some(open) = self.bubbles.as_ref() else { return };
        self.bubbles_input(&open.layer, &open.spans, wanted.0);
        let Some(open) = self.bubbles.as_mut() else { return };
        match plan_redraw(open.size, wanted, open.configured) {
            Redraw::Resize(w, h) => {
                open.size = (w, h);
                open.layer.set_size(w, h);
                open.layer.commit();
                open.last = None;
            }
            Redraw::Draw => self.bubbles_draw(),
            Redraw::Wait => open.layer.commit(),
        }
    }

    /// Paints the stack: the gaps transparent, each bubble a boxed surface of its own rows (`urgent`
    /// frame when critical) in its band of the buffer. An unchanged stack commits nothing.
    fn bubbles_draw(&mut self) {
        let height = self.bar.config.height;
        let App { bubbles, fonts, roles, .. } = self;
        let Some(open) = bubbles.as_mut() else { return };
        if !open.configured || open.last.as_ref() == Some(&open.stack) {
            return;
        }
        let e = open.scale.max(1);
        let ue = e as u32;
        let row_height = height * ue;
        let (w, h) = (open.size.0 * ue, open.size.1 * ue);
        let OpenBubbles { buffers, pool, layer, stack, spans, .. } = open;
        let Some(used) = pick_buffer(buffers, pool, w, h) else { return };
        let Some(px) = buffers[used].0.canvas(pool) else {
            log(&format!("bubbles buffer {w}×{h}: unexpectedly busy"));
            return;
        };
        px.fill(0);
        let stride = w as usize * 4;
        for (b, &(top, tall)) in stack.iter().zip(spans.iter()) {
            let (y0, rows) = (top * ue, tall * ue);
            // A bubble past an imposed height is not painted (never a panic).
            let Some(band) = px.get_mut(y0 as usize * stride..(y0 + rows) as usize * stride) else { break };
            let ink = if b.critical { FrameInk::Urgent } else { FrameInk::Border };
            fonts.paint_boxed(&b.boxed, roles, band, w, rows, row_height, e, ink);
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        layer.commit();
        let shown = stack.len();
        open.last = Some(open.stack.clone());
        log(&format!("bubbles shown: {shown}"));
    }

    fn bubbles_drop(&mut self, why: &str) {
        let Some(open) = self.bubbles.take() else { return };
        drop(open);
        log(&format!("bubbles surface dropped ({why})"));
        // The pool and its buffers: give the pages back.
        crate::memory::release_memory();
    }

    /// Whether `surface` is the bubbles' (checked by identity, before the bar's own handling).
    pub(crate) fn bubbles_owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.bubbles.as_ref().is_some_and(|o| o.layer.wl_surface() == surface)
    }

    /// The bubbles' configure (the caller checked `bubbles_owns`): an imposed size is obeyed (the
    /// bubbles that do not fit are not painted), then drawn.
    pub(crate) fn bubbles_configured(&mut self, size: (u32, u32)) {
        let Some(open) = self.bubbles.as_mut() else { return };
        if let Some(imposed) = plan_configure(open.size, size) {
            open.size = imposed;
            // The input region follows the width the bubbles are painted at (the draw commits it).
            let Some(open) = self.bubbles.as_ref() else { return };
            self.bubbles_input(&open.layer, &open.spans, imposed.0);
        }
        let Some(open) = self.bubbles.as_mut() else { return };
        open.configured = true;
        open.last = None;
        self.bubbles_draw();
    }

    /// The compositor closed the bubbles' surface (its output going, or its own reasons): dropped, then
    /// decided once the dispatch batch is over (the output list is up to date by then, and an output
    /// gone in the batch makes it an unplug, `Closes`): opened again, once per stack for the
    /// compositor's own closes.
    pub(crate) fn bubbles_layer_closed(&mut self) {
        let keys = self.bubbles.as_ref().map(|o| keys(&o.stack)).unwrap_or_default();
        self.bubbles_drop("closed by the compositor");
        self.bubble_closes.closed(keys);
        if let Some(handle) = &self.loop_handle {
            handle.insert_idle(|app| match app.bubble_closes.settle() {
                Some(true) => app.bubbles_sync(),
                Some(false) => log("bubbles: closed again by the compositor; shown again when they change"),
                None => {}
            });
        }
    }

    /// An output went away: when it holds the bubbles, they are dropped and shown again after the
    /// batch (sctk still lists the output during `output_destroyed`), on the focused output or the
    /// compositor's choice.
    pub(crate) fn bubbles_output_gone(&mut self, output: &wl_output::WlOutput) {
        // A close earlier in this batch was this unplug's doing.
        self.bubble_closes.output_lost();
        if !self.bubbles.as_ref().is_some_and(|o| bubbles::holds(o.output.as_ref(), output)) {
            return;
        }
        self.bubbles_drop("output gone");
        if let Some(handle) = &self.loop_handle {
            handle.insert_idle(|app| app.bubbles_sync());
        }
    }

    /// A scale change of the bubbles' surface: laid out and drawn again at the new scale (resized when
    /// the logical size moved); false: not ours.
    pub(crate) fn bubbles_scale(&mut self, surface: &wl_surface::WlSurface, factor: i32) -> bool {
        if !self.bubbles_owns(surface) {
            return false;
        }
        let Some((stack, scale)) = self.bubbles.as_ref().map(|o| (o.stack.clone(), o.scale)) else { return true };
        if scale == factor {
            return true;
        }
        let (spans, size) = self.bubbles_layout(&stack, factor);
        if let Some(open) = self.bubbles.as_mut() {
            open.scale = factor;
            open.spans = spans;
            open.last = None;
        }
        log(&format!("bubbles scale {factor}"));
        self.bubbles_resize(size);
        true
    }

    /// Pointer events on the bubbles; false: not ours. Left: `notify_activate`; right: dismissed.
    pub(crate) fn bubbles_pointer(&mut self, conn: &Connection, ev: &PointerEvent) -> bool {
        if !self.bubbles_owns(&ev.surface) {
            return false;
        }
        match &ev.kind {
            PointerEventKind::Enter { .. } => {
                if let Some(p) = &self.pointer {
                    let _ = p.set_cursor(conn, CursorIcon::Default);
                }
            }
            PointerEventKind::Press { button, .. } => {
                let under = self
                    .bubbles
                    .as_ref()
                    .and_then(|o| bubbles::hit(&o.spans, ev.position.1).and_then(|i| o.stack.get(i)).map(|b| (b.id, b.revision)));
                match (under, actions::button(*button)) {
                    (Some((id, revision)), Some(Button::Left)) => self.notify_activate(id, revision),
                    (Some((id, _)), Some(Button::Right)) => self.notify_dismiss(id),
                    _ => {}
                }
            }
            _ => {}
        }
        true
    }

    /// The expiry timer follows the earliest finite expiry of a live bubble, shown or hidden by
    /// do-not-disturb (spec § 3; constraints, Lightness): a hidden one still closes on time, and never
    /// shows up later past its time. None while nothing expires (sticky critical bubbles, no bubble,
    /// the name not held).
    pub(crate) fn bubble_timer_sync(&mut self) {
        let deadline = notifications_of(self.tray.as_ref(), self.link.as_ref()).filter(|n| n.held()).and_then(|n| n.center().next_expiry());
        if let Some(handle) = self.loop_handle.clone() {
            self.bubble_timer.sync(&handle, deadline, App::bubble_expire);
        }
    }

    /// The timer fired (calloop callback; its source is dropped). Expired at the real time, never
    /// before the deadline it was armed for: a bubble due at that instant always goes, so the timer
    /// never re-arms on the same deadline.
    pub(crate) fn bubble_expire(app: &mut App, deadline: Instant) {
        app.bubble_timer.fired();
        let now = Instant::now().max(deadline);
        let mut expired = 0;
        app.notify_act(|n| {
            n.act(|c| {
                let effects = c.expire(now);
                expired = effects.len();
                effects
            })
        });
        if expired > 0 {
            log(&format!("bubbles: {expired} expired"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A close, then the batch over: Some(reopen).
    fn close(c: &mut Closes, keys: &[(u32, u32)]) -> Option<bool> {
        c.closed(keys.to_vec());
        c.settle()
    }

    #[test]
    fn a_compositor_close_reopens_once_per_stack() {
        let mut c = Closes::default();
        let (a, b) = (vec![(1, 1)], vec![(1, 1), (2, 1)]);
        assert!(c.may_open(&a));
        assert_eq!(close(&mut c, &a), Some(true), "the first close: open it again");
        assert!(c.may_open(&a));
        assert_eq!(close(&mut c, &a), Some(false), "closed again for the same stack: leave it");
        assert!(!c.may_open(&a), "not until the stack changes");
        assert!(c.may_open(&b), "a new stack opens");
        assert!(c.may_open(&a), "and the hold is gone");
        assert_eq!(close(&mut c, &b), Some(true), "another stack: reopened once too");
        assert_eq!(c.settle(), None, "settled once");
    }

    /// A close followed by an output going, in the same batch, is the output's loss (an unplug):
    /// reopened, and never counted as the stack's one reopen, however many unplugs.
    #[test]
    fn a_close_from_an_unplug_is_not_the_stacks_one_reopen() {
        let mut c = Closes::default();
        let a = vec![(1, 1)];
        c.output_lost();
        assert_eq!(c.settle(), None, "an output gone with nothing closed: nothing to settle");
        for _ in 0..3 {
            c.closed(a.clone());
            c.output_lost();
            assert_eq!(c.settle(), Some(true), "an unplug: open it again");
            assert!(c.may_open(&a));
        }
        assert_eq!(close(&mut c, &a), Some(true), "the compositor's own first close still reopens");
        assert_eq!(close(&mut c, &a), Some(false), "its second leaves it closed");
    }
}
