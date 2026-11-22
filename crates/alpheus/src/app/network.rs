use super::action::{Command, External};
use super::{Item, ToggleId};

pub(super) fn items() -> Vec<Item> {
    vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))]
}
