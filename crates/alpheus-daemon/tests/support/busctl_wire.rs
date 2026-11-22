//! busctl's JSON (`busctl --json=short`, tests/fixtures/dbus-*.json) turned
//! back into D-Bus message arguments, for the tests only: a recorded reply
//! marshalled again, as the service sent it. Shared by the unit tests of
//! `src/sources/dbus/dbusrs_bus.rs` and by the private bus's recorded
//! workstation (`tests/common`).

use dbus::Message;
use dbus::arg::IterAppend;
use dbus::strings::{Path, Signature};
use serde_json::Value;

fn signature(s: &str) -> Signature<'static> {
    Signature::new(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// Appends `v`, of D-Bus type `sig` (one complete type), as busctl printed
/// it (a variant is `{"type": …, "data": …}`).
pub fn append(i: &mut IterAppend<'_>, sig: &str, v: &Value) {
    let int = || v.as_i64().unwrap_or_else(|| panic!("{sig}: {v}"));
    match sig {
        "b" => i.append(v.as_bool().expect("b")),
        "y" => i.append(int() as u8),
        "n" => i.append(int() as i16),
        "q" => i.append(int() as u16),
        "i" => i.append(int() as i32),
        "u" => i.append(int() as u32),
        "x" => i.append(int()),
        "t" => i.append(v.as_u64().expect("t")),
        "d" => i.append(v.as_f64().expect("d")),
        "s" => i.append(v.as_str().expect("s")),
        "o" => i.append(Path::new(v.as_str().expect("o")).expect("an object path")),
        "v" => {
            let inner = v["type"].as_str().expect("a variant's type");
            i.append_variant(&signature(inner), |s| append(s, inner, &v["data"]));
        }
        _ if sig.starts_with("a{") => {
            let (key, value) = (&sig[2..3], &sig[3..sig.len() - 1]);
            i.append_dict(&signature(key), &signature(value), |d| {
                for (k, x) in v.as_object().expect("a dict") {
                    d.append_dict_entry(|e| {
                        append(e, key, &Value::from(k.as_str()));
                        append(e, value, x);
                    });
                }
            });
        }
        _ if sig.starts_with('a') => {
            let element = &sig[1..];
            i.append_array(&signature(element), |a| {
                for x in v.as_array().expect("an array") {
                    append(a, element, x);
                }
            });
        }
        other => panic!("{other}: not in the recordings"),
    }
}

/// Appends a recorded reply's single argument (`{"type": …, "data": [arg]}`)
/// to `m`.
pub fn push_reply(m: &mut Message, json: &str) {
    let v: Value = serde_json::from_str(json).expect("busctl JSON");
    append(&mut IterAppend::new(m), v["type"].as_str().expect("type"), &v["data"][0]);
}
