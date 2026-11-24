//! The Sound tab's state: which outputs and inputs it lists, and what choosing one does.
//!
//! The audio data (`App::data.audio`, read by the worker as `Domain::Audio`) lives
//! on `App` in `mod.rs`, and so do the slider helpers (`slider_value`, `set_slider` and the
//! others): a slider here is a `SliderId::Node`, one more kind beside the volume and brightness
//! sliders. This file holds what only this tab does: its list of items, the search filter on
//! them, and making a node the default.
//!
//! Never:
//! - run a tool: a change is queued for the worker by `send_op`; the worker is the only thing
//!   that runs `wpctl`;
//! - draw anything: `ui/sound.rs` draws the tab from the same items, in the same order.
//!
//! See: docs/features/control-center.md.

use super::action::{Command, External};
use super::worker::Op;
use super::{App, Item, Loadable, SliderId};
use crate::backends::AudioNode;

/// The focusable items of the Sound tab, in the order they are drawn: one slider per output that
/// passes the search, one per input that passes it, then the "Per-app mixer" button.
///
/// The button is always the last item: `ui/sound.rs` draws `items.len() - 1` after the two lists.
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
    /// When the audio data is loaded and holds `id`, the change shows at once, before the tool
    /// answers: the node is marked as default in the list that holds it, and the other nodes of
    /// that list are not. The other list keeps its own default, so choosing an output never
    /// changes the default input. Otherwise only the request is queued. While the change is in
    /// flight, any read of the domains `Op::domains` lists (Audio and Volume) is dropped
    /// (`on_update`), so a read does not undo what is shown.
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
