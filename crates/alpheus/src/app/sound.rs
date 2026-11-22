//! The Sound tab's state: which outputs and inputs it lists, and what choosing one does.
//!
//! The tab lists the audio outputs, then the inputs, each as a volume slider, then one "Per-app
//! mixer" button. The audio data (`App::data.audio`, read by the worker as `Domain::Audio`) lives
//! on `App` in `mod.rs`, and so do the slider helpers (`slider_value`, `set_slider` and the
//! others): a slider here is a `SliderId::Node`, one more kind beside the volume and brightness
//! sliders. This file holds what only this tab does: its list of items, the search filter on
//! them, and making a node the default.
//!
//! Never:
//! - run a tool: a change is queued as a `Request::Do(Op)` for the worker (`send_op`), which is
//!   the only thing that runs `wpctl`;
//! - draw anything: `ui/sound.rs` draws the tab from the same items, in the same order.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Sound row), docs/features/control-center.md.

use super::action::{Command, External};
use super::worker::Op;
use super::{App, Item, Loadable, SliderId};
use crate::backends::AudioNode;

/// The focusable items of the Sound tab, in the order they are drawn: one slider per output that
/// passes the search, one per input that passes it, then the "Per-app mixer" button.
///
/// `ui/sound.rs` counts on the button being last: it draws the last item after the two lists.
pub(super) fn items(app: &App) -> Vec<Item> {
    let (sinks, sources) = app.visible_nodes();
    let mut v: Vec<Item> = sinks.iter().chain(sources.iter()).map(|n| Item::Slider(SliderId::Node(n.id))).collect();
    v.push(Item::Button(Command::Open(External::Mixer)));
    v
}

impl App {
    /// The outputs, then the inputs, that pass the search (everything when the search is empty;
    /// otherwise a node passes if its name matches the search text, by `fuzzy::score`). Both lists
    /// are empty until the audio data has loaded, and when its read failed.
    pub fn visible_nodes(&self) -> (Vec<&AudioNode>, Vec<&AudioNode>) {
        match self.data.audio.ready() {
            None => (Vec::new(), Vec::new()),
            Some(a) => (
                a.sinks.iter().filter(|n| self.matches_search(&n.name)).collect(),
                a.sources.iter().filter(|n| self.matches_search(&n.name)).collect(),
            ),
        }
    }

    /// Makes the output or input `id` the default and queues `Op::SetDefault` for the worker.
    ///
    /// The change shows at once, before the tool answers: the node is marked as default in the
    /// list that holds it, and the other nodes of that list are not. The other list keeps its
    /// own default, so choosing an output never changes the default input. While the change is in
    /// flight, a read of the audio data that arrives is dropped (`on_update`, for the domains
    /// `Op::domains` lists), so a read that started before the change does not undo it.
    pub(super) fn set_default(&mut self, id: u32) {
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
}
