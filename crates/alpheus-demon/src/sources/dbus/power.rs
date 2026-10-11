//! power-profiles-daemon (quick spec § 5): the active profile and the profiles it offers, read from
//! `net.hadess.PowerProfiles`'s one object through its owner's unique name, as every reader here
//! does. Read only: the quick panel sets a profile with `powerprofilesctl`.

use super::{Bus, POWER_PROFILES};
use crate::system::{Profile, ProfileState};
use std::io;

pub const PATH: &str = "/net/hadess/PowerProfiles";
pub const INTERFACE: &str = "net.hadess.PowerProfiles";

pub fn read(bus: &mut dyn Bus) -> io::Result<ProfileState> {
    let Some(owner) = bus.owner(POWER_PROFILES)? else { return Ok(ProfileState::default()) };
    let p = bus.get_all(&owner, PATH, INTERFACE)?;
    let mut offered = Vec::new();
    for d in p.dicts("Profiles") {
        if let Some(profile) = d.str("Profile").and_then(Profile::parse)
            && !offered.contains(&profile)
        {
            offered.push(profile);
        }
    }
    Ok(ProfileState { active: p.str("ActiveProfile").and_then(Profile::parse), offered })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::fake::{FakeBus, POWER_OWNER};
    use crate::sources::dbus::props::{Prop, Props};

    #[test]
    fn the_active_profile_and_the_offered_ones_through_the_owner() {
        let mut bus = FakeBus::empty();
        assert_eq!(read(&mut bus).unwrap(), ProfileState::default(), "no daemon: the row hidden");
        assert_eq!(bus.calls, ["GetNameOwner net.hadess.PowerProfiles"], "never a call to a name nobody owns: it would start it");
        bus.with_power_profiles("performance");
        assert_eq!(read(&mut bus).unwrap(), ProfileState { active: Some(Profile::Performance), offered: Profile::ALL.to_vec() });
        assert_eq!(bus.calls.last().cloned(), Some(format!("GetAll {POWER_OWNER} {PATH} {INTERFACE}")));
    }

    /// Decision 12: only the three known profiles, each once; an unknown active one hides the row.
    #[test]
    fn unknown_or_repeated_profiles_are_left_out() {
        let mut bus = FakeBus::empty();
        bus.with_power_profiles("turbo");
        let d = |p: &str| Props([("Profile".to_string(), Prop::Str(p.into()))].into());
        bus.props_mut(PATH, INTERFACE)
            .0
            .insert("Profiles".into(), Prop::Dicts(vec![d("balanced"), d("turbo"), d("balanced"), d("power-saver")]));
        assert_eq!(read(&mut bus).unwrap(), ProfileState { active: None, offered: vec![Profile::Balanced, Profile::PowerSaver] });
    }
}
