//! VPN profiles as the quick panel reads them from NetworkManager's `GetSettings`: name, uuid,
//! kind, last use and whether a secret is asked at connection time. No secret is ever decoded.

use super::props::Props;
use super::wifi::valid_uuid;
use crate::system::{VpnKind, VpnProfile};
use crate::text;

/// The longest profile name kept, in bytes (NetworkManager's own limit).
pub const NAME_MAX: usize = 255;
/// The most active VPN connections kept.
pub const MAX_ACTIVE: usize = 16;

/// NMSettingSecretFlags: a secret is asked at connection time when it is agent-owned (1) or
/// not saved (2) and not marked not-required (4).
pub fn secret_asked(flags: u32) -> bool {
    flags & 4 == 0 && flags & 3 != 0
}

/// A saved connection's `connection` setting as a VPN profile; None for any other kind, or a uuid
/// nmcli would read as an option. The name is cleaned and cut on a character boundary.
pub fn saved_vpn(connection: Option<&Props>, asks: bool) -> Option<VpnProfile> {
    let c = connection?;
    let kind = match c.str("type")? {
        "vpn" => VpnKind::Plugin,
        "wireguard" => VpnKind::WireGuard,
        _ => return None,
    };
    let uuid = c.str("uuid").filter(|u| valid_uuid(u))?.to_string();
    let mut name = text::clean(c.str("id").unwrap_or(""));
    if name.len() > NAME_MAX {
        let mut end = NAME_MAX;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    if name.is_empty() {
        name.push('?');
    }
    let last_used = c.int("timestamp").and_then(|t| u64::try_from(t).ok()).unwrap_or(0);
    Some(VpnProfile { uuid, name, kind, last_used, asks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::props::busctl as fixture;

    fn connection(json: &str) -> Props {
        fixture::get_all(&format!(r#"{{"type":"a{{sv}}","data":[{json}]}}"#))
    }

    #[test]
    fn a_hostile_name_is_cleaned_cut_and_never_empty() {
        let with = |id: &str| {
            let id = serde_json::to_string(id).unwrap();
            let json = format!(
                r#"{{"id":{{"type":"s","data":{id}}},"uuid":{{"type":"s","data":"11111111-2222-4333-8444-555555555555"}},"type":{{"type":"s","data":"vpn"}}}}"#
            );
            saved_vpn(Some(&connection(&json)), false).unwrap().name
        };
        assert_eq!(with("\u{202e}a\u{7}b"), "ab");
        let long = with(&"é".repeat(300));
        assert!(long.len() <= NAME_MAX && !long.is_empty());
        assert_eq!(with("\u{7}"), "?");
        assert_eq!(with(""), "?", "an empty id");
        let bad = connection(r#"{"uuid":{"type":"s","data":"-x"},"type":{"type":"s","data":"vpn"}}"#);
        assert_eq!(saved_vpn(Some(&bad), false), None);
    }

    #[test]
    fn last_use_is_zero_when_absent_or_negative() {
        let with = |ts: &str| {
            let json = format!(
                r#"{{"uuid":{{"type":"s","data":"11111111-2222-4333-8444-555555555555"}},"type":{{"type":"s","data":"vpn"}}{ts}}}"#
            );
            saved_vpn(Some(&connection(&json)), false).unwrap().last_used
        };
        assert_eq!(with(""), 0, "absent");
        assert_eq!(with(r#","timestamp":{"type":"x","data":-5}"#), 0, "negative");
        assert_eq!(with(r#","timestamp":{"type":"t","data":1700000000}"#), 1700000000);
    }

    #[test]
    fn secret_flags() {
        assert!(secret_asked(1) && secret_asked(2) && secret_asked(3));
        assert!(!secret_asked(0) && !secret_asked(4) && !secret_asked(5));
    }
}
