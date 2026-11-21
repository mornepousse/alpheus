//! `systemctl list-units --failed --output=json`.

use super::names::valid_unit;
use crate::text::clean_line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedUnit {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

/// The most units kept.
pub const MAX_UNITS: usize = 256;

/// The failed units of `list-units --failed --output=json`, at most MAX_UNITS (see `parse_units`).
pub fn parse_failed(json: &[u8]) -> Result<Vec<FailedUnit>, String> {
    parse_units(json, MAX_UNITS)
}

/// The units of `list-units --output=json`, at most `max`; a unit whose name fails `valid_unit`
/// is dropped, every other text cleaned to 200 characters.
pub fn parse_units(json: &[u8], max: usize) -> Result<Vec<FailedUnit>, String> {
    let list: Vec<serde_json::Value> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let text = |v: &serde_json::Value, key: &str| clean_line(v.get(key).and_then(|x| x.as_str()).unwrap_or(""), 200);
    Ok(list
        .iter()
        .filter_map(|v| {
            // The raw name is checked, not a cleaned (cut) one: a valid name is plain ASCII, so there
            // is nothing to clean, and a 201..=256 byte unit must not vanish.
            let unit = v.get("unit").and_then(|x| x.as_str()).unwrap_or("");
            valid_unit(unit).then(|| FailedUnit {
                unit: unit.to_string(),
                load: text(v, "load"),
                active: text(v, "active"),
                sub: text(v, "sub"),
                description: text(v, "description"),
            })
        })
        .take(max)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_valid_unit_name_is_kept_whole() {
        let name = format!("{}.service", "a".repeat(240));
        let json = format!(r#"[{{"unit":"{name}","load":"loaded","active":"failed","sub":"failed","description":"d"}}]"#);
        let units = parse_failed(json.as_bytes()).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].unit, name);
    }

    #[test]
    fn failed_units_from_recorded_json() {
        let units = parse_failed(include_bytes!("../../tests/fixtures/diag-failed-system.json")).unwrap();
        assert_eq!(units.len(), 3);
        assert_eq!(
            units[0],
            FailedUnit {
                unit: "example-sync.service".into(),
                load: "loaded".into(),
                active: "failed".into(),
                sub: "failed".into(),
                description: "Example sync job".into(),
            }
        );
        assert_eq!(units[1].unit, "mnt-data.mount");
        assert_eq!(units[2].description, "Example app with hostile text", "controls and bidi removed");
        assert_eq!(parse_failed(include_bytes!("../../tests/fixtures/diag-failed-empty.json")), Ok(vec![]));
        assert!(parse_failed(b"not json").is_err());
        assert!(parse_failed(b"{}").is_err(), "an object is not a list");
    }

    #[test]
    fn units_with_invalid_names_are_dropped_and_the_list_is_bounded() {
        let json = br#"[{"unit":"--now","active":"failed"},{"unit":"a b.service"},{"unit":"-.mount","load":"loaded","active":"failed","sub":"failed"},{"description":"no unit"}]"#;
        let units = parse_failed(json).unwrap();
        assert_eq!(units.iter().map(|u| u.unit.as_str()).collect::<Vec<_>>(), ["-.mount"]);
        let many: Vec<String> = (0..300).map(|i| format!(r#"{{"unit":"u{i}.service"}}"#)).collect();
        let big = format!("[{}]", many.join(","));
        assert_eq!(parse_failed(big.as_bytes()).unwrap().len(), MAX_UNITS);
        let long = format!(r#"[{{"unit":"a.service","description":"{}"}}]"#, "d".repeat(300));
        assert_eq!(parse_failed(long.as_bytes()).unwrap()[0].description.chars().count(), 200);
    }
}
