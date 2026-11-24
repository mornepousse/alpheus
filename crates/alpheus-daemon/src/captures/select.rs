//! The area selector's model: what a pointer and a keyboard make of a selection on one output.
//!
//! This is the pure half of the selector. A `Selection` is told about the pointer pressing, moving
//! and letting go, and about the keys (`SelInput`), and answers with `SelState`: still selecting,
//! done with a `Rect`, or cancelled. The other half, `select_app.rs`, owns the layer surface, turns
//! Wayland events into `SelInput` and draws `Selection::rect` and `Selection::label`. All
//! coordinates are logical pixels of the one output the selector covers.
//!
//! The gestures are slurp's, with a few additions: a drag selects and its release confirms; a
//! click without a drag cancels; Escape cancels; Space held moves the selection instead of
//! resizing it. Enter confirms the rectangle being dragged; the arrow keys nudge it by 1 px (10 px
//! with Shift), and with Ctrl they resize it, so a button held down can be given pixel precision.
//!
//! Never:
//! - give a rectangle outside the output, or with a zero width or height: such a selection is a
//!   cancel (`a_zero_area_is_a_cancel`, `an_area_past_the_edge_is_clamped`);
//! - read a clock, a file or the Wayland connection: the caller hands in every event;
//! - change its answer once it has given `Done` or `Cancelled` (the end is latched).
//!
//! See: docs/features/captures.md and docs/decisions.md ("A capture never touches the clipboard
//! unless it succeeded").

use super::Rect;
use crate::keys::Mods;

/// Pixels an arrow key moves the selection, and with Shift.
const NUDGE: i32 = 1;
const NUDGE_SHIFT: i32 = 10;

/// The keys the selector answers to, as the surface turns them from keysyms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelKey {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Escape,
    /// Space went down: while it is held the selection moves instead of resizing.
    SpaceDown,
    SpaceUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelInput {
    Press(i32, i32),
    Motion(i32, i32),
    Release,
    Key(SelKey, Mods),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelState {
    Selecting,
    Done(Rect),
    Cancelled,
}

/// A drag in progress: where the button went down, and the corner that follows the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Drag {
    anchor: (i32, i32),
    corner: (i32, i32),
}

impl Drag {
    /// The rectangle between the two corners; None when it has no area.
    fn rect(&self) -> Option<Rect> {
        let (x0, x1) = (self.anchor.0.min(self.corner.0), self.anchor.0.max(self.corner.0));
        let (y0, y1) = (self.anchor.1.min(self.corner.1), self.anchor.1.max(self.corner.1));
        let (width, height) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if width == 0 || height == 0 {
            return None;
        }
        Some(Rect { x: x0, y: y0, width, height })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Logical size of the output: every coordinate is kept within `0..=bounds`.
    bounds: (i32, i32),
    drag: Option<Drag>,
    space: bool,
    /// The last pointer position, so that a motion with Space held is a move by the difference.
    pointer: (i32, i32),
    end: SelState,
}

impl Selection {
    /// A selection on an output of `width` × `height` logical px.
    pub fn new(width: u32, height: u32) -> Selection {
        let bounds = (width.min(i32::MAX as u32) as i32, height.min(i32::MAX as u32) as i32);
        Selection { bounds, drag: None, space: false, pointer: (0, 0), end: SelState::Selecting }
    }

    /// The rectangle being dragged, None while there is none or it has no area.
    pub fn rect(&self) -> Option<Rect> {
        self.drag.and_then(|d| d.rect())
    }

    /// `W×H` of the rectangle being dragged, for the label.
    pub fn label(&self) -> Option<String> {
        self.rect().map(|r| format!("{}×{}", r.width, r.height))
    }

    /// Applies one input and gives the state after it.
    pub fn apply(&mut self, input: SelInput) -> SelState {
        if self.end != SelState::Selecting {
            return self.end;
        }
        match input {
            SelInput::Press(x, y) => {
                // A second button while dragging changes nothing.
                if self.drag.is_none() {
                    let at = self.clamp((x, y));
                    self.drag = Some(Drag { anchor: at, corner: at });
                }
                self.pointer = (x, y);
            }
            SelInput::Motion(x, y) => {
                self.motion((x, y));
                self.pointer = (x, y);
            }
            SelInput::Release => {
                if let Some(drag) = self.drag {
                    self.finish(drag);
                }
            }
            SelInput::Key(key, mods) => self.key(key, mods),
        }
        self.end
    }

    fn motion(&mut self, to: (i32, i32)) {
        let Some(drag) = self.drag else { return };
        if self.space {
            self.shift((to.0 - self.pointer.0, to.1 - self.pointer.1));
        } else {
            self.drag = Some(Drag { corner: self.clamp(to), ..drag });
        }
    }

    fn key(&mut self, key: SelKey, mods: Mods) {
        let step = if mods.shift { NUDGE_SHIFT } else { NUDGE };
        match key {
            SelKey::Escape => self.end = SelState::Cancelled,
            SelKey::Enter => {
                if let Some(drag) = self.drag {
                    self.finish(drag);
                }
            }
            SelKey::SpaceDown => self.space = true,
            SelKey::SpaceUp => self.space = false,
            SelKey::Left => self.nudge((-step, 0), mods.ctrl),
            SelKey::Right => self.nudge((step, 0), mods.ctrl),
            SelKey::Up => self.nudge((0, -step), mods.ctrl),
            SelKey::Down => self.nudge((0, step), mods.ctrl),
        }
    }

    /// An arrow key: moves the rectangle, or with Ctrl moves the corner that follows the pointer.
    fn nudge(&mut self, by: (i32, i32), resize: bool) {
        let Some(drag) = self.drag else { return };
        if resize {
            let corner = self.clamp((drag.corner.0 + by.0, drag.corner.1 + by.1));
            self.drag = Some(Drag { corner, ..drag });
        } else {
            self.shift(by);
        }
    }

    /// Moves both corners by `by`, stopped where the rectangle meets the output's edge.
    fn shift(&mut self, by: (i32, i32)) {
        let Some(drag) = self.drag else { return };
        let (low_x, high_x) = (drag.anchor.0.min(drag.corner.0), drag.anchor.0.max(drag.corner.0));
        let (low_y, high_y) = (drag.anchor.1.min(drag.corner.1), drag.anchor.1.max(drag.corner.1));
        let dx = by.0.clamp(-low_x, self.bounds.0 - high_x);
        let dy = by.1.clamp(-low_y, self.bounds.1 - high_y);
        self.drag = Some(Drag { anchor: (drag.anchor.0 + dx, drag.anchor.1 + dy), corner: (drag.corner.0 + dx, drag.corner.1 + dy) });
    }

    /// The drag's end: its rectangle, or a cancel when it has no area (a click, a line).
    fn finish(&mut self, drag: Drag) {
        self.end = match drag.rect() {
            Some(rect) => SelState::Done(rect),
            None => SelState::Cancelled,
        };
    }

    fn clamp(&self, (x, y): (i32, i32)) -> (i32, i32) {
        (x.clamp(0, self.bounds.0), y.clamp(0, self.bounds.1))
    }
}

/// Logical px between the selection and its label.
pub const LABEL_GAP: i32 = 6;

/// Where the label's top left goes: under the rectangle's bottom left corner; above its top left
/// when that would leave the output at the bottom; inside it when neither fits. Always kept
/// within the output as far as the label's own size allows.
pub fn label_origin(rect: Rect, label: (u32, u32), bounds: (u32, u32)) -> (i32, i32) {
    let (lw, lh) = (label.0 as i32, label.1 as i32);
    let (bw, bh) = (bounds.0 as i32, bounds.1 as i32);
    let x_for = |x: i32| x.min(bw - lw).max(0);
    let below = rect.y + rect.height as i32 + LABEL_GAP;
    if below + lh <= bh {
        return (x_for(rect.x), below);
    }
    let above = rect.y - LABEL_GAP - lh;
    if above >= 0 {
        return (x_for(rect.x), above);
    }
    (x_for(rect.x + LABEL_GAP), (rect.y + LABEL_GAP).min(bh - lh).max(0))
}

#[cfg(test)]
mod tests;
