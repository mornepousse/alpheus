use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Loadable};

pub(super) fn rows(app: &App, rail: bool, rows: &mut Vec<Row>) {
    rows.push(Row::Item(0));
    rows.push(Row::Blank);
    rows.push(Row::Heading("Active connections"));
    match &app.data.network {
        Loadable::Loading => rows.push(Row::Text("…".into())),
        Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
        Loadable::Ready(n) => {
            let list: Vec<_> = n.active.iter().filter(|c| app.matches_search(&c.name)).collect();
            if list.is_empty() {
                rows.push(Row::Text("none".into()));
            }
            for c in list {
                rows.push(Row::Text(if rail {
                    let glyph = match c.kind.as_str() {
                        "Wi-Fi" => "󰖩",
                        "Ethernet" => "󰈀",
                        "WireGuard" | "VPN" => "󰦝",
                        _ => "󰛳",
                    };
                    format!("{glyph}  {:<24} {:<10} {}", c.name, c.kind, c.device)
                } else {
                    format!("{:<10} {}  ({})", c.kind, c.name, c.device)
                }));
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
