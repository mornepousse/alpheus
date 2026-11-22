use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Item, Loadable};

pub(super) fn rows(app: &App, items: &[Item], rows: &mut Vec<Row>) {
    let (sinks, sources) = app.visible_nodes();
    let mut i = 0;
    for (title, count) in [("Outputs", sinks.len()), ("Inputs", sources.len())] {
        rows.push(Row::Heading(title));
        match &app.data.audio {
            Loadable::Loading => rows.push(Row::Text("…".into())),
            Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
            Loadable::Ready(_) if count == 0 => rows.push(Row::Text("none".into())),
            Loadable::Ready(_) => {
                for _ in 0..count {
                    rows.push(Row::Item(i));
                    i += 1;
                }
            }
        }
        rows.push(Row::Blank);
    }
    // The "Per-app mixer" button is always the last item of the
    // Sound tab (App::items()); subtraction is guarded so the
    // render never panics even if that invariant were to change.
    rows.push(Row::Item(items.len().saturating_sub(1)));
}

pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap, adjust: String) {
    pairs.push((km.label(Action::Activate), "Default"));
    pairs.push((adjust, "Volume"));
    pairs.push((km.label(Action::Search), "Search"));
}

#[cfg(test)]
mod tests;
