//! D-Bus property values, reduced to what the bar reads. The D-Bus layer
//! (`dbusrs_bus.rs`) converts into this; the tests build it from busctl's
//! JSON (`busctl --json=short`), the replies recorded on the workstation.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    Bool(bool),
    /// Every integer type (y, n, q, i, u, x, t).
    Int(i64),
    Float(f64),
    Str(String),
    Path(String),
    Paths(Vec<String>),
    Bytes(Vec<u8>),
    /// Anything else (dictionaries, string arrays…): never read by the bar.
    Other,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Props(pub BTreeMap<String, Prop>);

impl Props {
    pub fn bool(&self, k: &str) -> Option<bool> {
        match self.0.get(k)? {
            Prop::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn int(&self, k: &str) -> Option<i64> {
        match self.0.get(k)? {
            Prop::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn float(&self, k: &str) -> Option<f64> {
        match self.0.get(k)? {
            Prop::Float(f) => Some(*f),
            Prop::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn str(&self, k: &str) -> Option<&str> {
        match self.0.get(k)? {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }

    /// An object path; "/" (NetworkManager's "nothing") reads as None.
    pub fn path(&self, k: &str) -> Option<&str> {
        match self.0.get(k)? {
            Prop::Path(p) if p != "/" => Some(p),
            _ => None,
        }
    }

    pub fn paths(&self, k: &str) -> &[String] {
        match self.0.get(k) {
            Some(Prop::Paths(v)) => v,
            _ => &[],
        }
    }

    pub fn bytes(&self, k: &str) -> Option<&[u8]> {
        match self.0.get(k)? {
            Prop::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// An ObjectManager's objects: path → interface → properties.
pub type Objects = BTreeMap<String, BTreeMap<String, Props>>;

#[cfg(test)]
pub mod busctl {
    //! The JSON printed by `busctl --system --json=short call …`, as recorded
    //! in tests/fixtures/dbus-*.json.

    use super::{Objects, Prop, Props};
    use serde_json::Value;

    fn prop(v: &Value) -> Prop {
        let data = &v["data"];
        match v["type"].as_str().unwrap_or("") {
            "b" => data.as_bool().map(Prop::Bool).unwrap_or(Prop::Other),
            "y" | "n" | "q" | "i" | "u" | "x" | "t" => data.as_i64().map(Prop::Int).unwrap_or(Prop::Other),
            "d" => data.as_f64().map(Prop::Float).unwrap_or(Prop::Other),
            "s" => data.as_str().map(|s| Prop::Str(s.into())).unwrap_or(Prop::Other),
            "o" => data.as_str().map(|s| Prop::Path(s.into())).unwrap_or(Prop::Other),
            "ao" => Prop::Paths(data.as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect()),
            "ay" => {
                Prop::Bytes(data.as_array().into_iter().flatten().filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect())
            }
            _ => Prop::Other,
        }
    }

    fn props_of(map: &Value) -> Props {
        Props(map.as_object().into_iter().flatten().map(|(k, v)| (k.clone(), prop(v))).collect())
    }

    fn parse(json: &str) -> Value {
        serde_json::from_str(json).expect("busctl JSON")
    }

    /// Reply of `… GetAll s <interface>`: `{"type":"a{sv}","data":[{…}]}`.
    pub fn get_all(json: &str) -> Props {
        props_of(&parse(json)["data"][0])
    }

    /// Reply of `… GetManagedObjects`: `{"type":"a{oa{sa{sv}}}","data":[{path:{interface:{…}}}]}`.
    pub fn objects(json: &str) -> Objects {
        let v = parse(json);
        v["data"][0]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(path, interfaces)| {
                (path.clone(), interfaces.as_object().into_iter().flatten().map(|(i, p)| (i.clone(), props_of(p))).collect())
            })
            .collect()
    }

    /// Reply of a method returning `ao`: `{"type":"ao","data":[[…]]}`.
    pub fn paths(json: &str) -> Vec<String> {
        parse(json)["data"][0].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busctl_replies_read_as_props() {
        let p = busctl::get_all(
            r#"{"type":"a{sv}","data":[{"On":{"type":"b","data":true},"N":{"type":"u","data":70},"P":{"type":"d","data":92.5},"S":{"type":"s","data":"x"},"O":{"type":"o","data":"/a"},"None":{"type":"o","data":"/"},"L":{"type":"ao","data":["/a","/b"]},"B":{"type":"ay","data":[77,79]},"Map":{"type":"a{sv}","data":{}}}]}"#,
        );
        assert_eq!(p.bool("On"), Some(true));
        assert_eq!(p.int("N"), Some(70));
        assert_eq!(p.float("P"), Some(92.5));
        assert_eq!(p.float("N"), Some(70.0), "an integer reads as a float too");
        assert_eq!(p.str("S"), Some("x"));
        assert_eq!(p.path("O"), Some("/a"));
        assert_eq!(p.path("None"), None, "\"/\" is NetworkManager's nothing");
        assert_eq!(p.paths("L"), ["/a", "/b"]);
        assert_eq!(p.bytes("B"), Some(&b"MO"[..]));
        assert_eq!(p.0.get("Map"), Some(&Prop::Other));
        assert_eq!((p.bool("N"), p.str("missing"), p.paths("S")), (None, None, &[][..]), "wrong type or missing: nothing");
    }

    #[test]
    fn recorded_objects_and_paths() {
        let o = busctl::objects(include_str!("../../../tests/fixtures/dbus-bluez-objects.json"));
        assert_eq!(o["/org/bluez/hci0"]["org.bluez.Adapter1"].bool("Powered"), Some(true));
        assert_eq!(o["/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02"]["org.bluez.Device1"].str("Alias"), Some("CasqueBT"));
        let d = busctl::paths(include_str!("../../../tests/fixtures/dbus-upower-devices.json"));
        assert_eq!(d.len(), 6);
        assert_eq!(d[5], "/org/freedesktop/UPower/devices/battery_hidpp_battery_1");
    }
}
