use super::action::{Command, External};
use super::worker::Op;
use super::{App, Item, Loadable, SliderId};
use crate::backends::AudioNode;

pub(super) fn items(app: &App) -> Vec<Item> {
    let (sinks, sources) = app.visible_nodes();
    let mut v: Vec<Item> = sinks.iter().chain(sources.iter()).map(|n| Item::Slider(SliderId::Node(n.id))).collect();
    v.push(Item::Button(Command::Open(External::Mixer)));
    v
}

impl App {
    /// Outputs then inputs, filtered by the search.
    pub fn visible_nodes(&self) -> (Vec<&AudioNode>, Vec<&AudioNode>) {
        match self.data.audio.ready() {
            None => (Vec::new(), Vec::new()),
            Some(a) => (
                a.sinks.iter().filter(|n| self.matches_search(&n.name)).collect(),
                a.sources.iter().filter(|n| self.matches_search(&n.name)).collect(),
            ),
        }
    }

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
