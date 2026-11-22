use crate::app::action::Action;
use crate::app::keymap::Keymap;

pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap) {
    pairs.push((km.label(Action::Activate), "Activate"));
}

#[cfg(test)]
mod tests;
