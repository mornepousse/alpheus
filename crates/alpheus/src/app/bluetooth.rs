use super::action::{Command, External};
use super::{Item, ToggleId};

pub(super) fn items() -> Vec<Item> {
    vec![Item::Toggle(ToggleId::Bluetooth), Item::Button(Command::Open(External::Bluetooth))]
}
