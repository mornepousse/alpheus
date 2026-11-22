use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Loadable};

pub(super) fn rows(app: &App, rail: bool, rows: &mut Vec<Row>) {
    rows.push(Row::Item(0));
    rows.push(Row::Blank);
    rows.push(Row::Heading("Connected devices"));
    match &app.data.bluetooth {
        Loadable::Loading => rows.push(Row::Text("…".into())),
        Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
        Loadable::Ready(b) if !b.powered => rows.push(Row::Text("Bluetooth is off".into())),
        Loadable::Ready(b) => {
            let list: Vec<_> = b.connected.iter().filter(|d| app.matches_search(&d.name)).collect();
            if list.is_empty() {
                rows.push(Row::Text("none".into()));
            }
            for d in list {
                rows.push(Row::Text(if rail { format!("󰂱  {:<24} {}", d.name, d.mac) } else { format!("{}  ({})", d.name, d.mac) }));
            }
        }
    }
    rows.push(Row::Blank);
    rows.push(Row::Item(1));
}

pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap) {
    pairs.push((km.label(Action::Toggle), "Toggle"));
    pairs.push((km.label(Action::Activate), "Open"));
    pairs.push((km.label(Action::Search), "Search"));
}

#[cfg(test)]
mod tests;
