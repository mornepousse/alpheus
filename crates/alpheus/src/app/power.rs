use super::Item;
use super::action::Command;
use crate::backends::PowerAction;

pub(super) fn items() -> Vec<Item> {
    PowerAction::ALL.iter().map(|a| Item::Button(Command::Power(*a))).collect()
}
