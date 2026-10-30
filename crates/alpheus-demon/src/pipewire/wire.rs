//! The shapes `pw-dump -m` prints, as far as alpheus reads them: every field not named here is
//! skipped, and a field of an unexpected type is absent, never a failed batch.

use serde::Deserialize;

/// One object of an array; every field this module doesn't name is skipped.
#[derive(Deserialize)]
pub struct Object {
    pub id: u32,
    /// Absent on a removed object.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub info: Option<Info>,
    /// Metadata objects: their name (`default`, `settings`…).
    #[serde(default)]
    pub props: Option<MetaProps>,
    #[serde(default)]
    pub metadata: Option<Vec<MetaEntry>>,
}

#[derive(Deserialize, Default)]
pub struct Info {
    #[serde(default, deserialize_with = "null_as_default")]
    pub props: NodeProps,
    #[serde(default, deserialize_with = "null_as_default")]
    pub params: Params,
}

#[derive(Deserialize, Default)]
pub struct NodeProps {
    #[serde(rename = "media.class", default, deserialize_with = "lenient_text")]
    pub media_class: Option<String>,
    #[serde(rename = "node.name", default, deserialize_with = "lenient_text")]
    pub node_name: Option<String>,
    #[serde(rename = "node.description", default, deserialize_with = "lenient_text")]
    pub description: Option<String>,
    #[serde(rename = "node.nick", default, deserialize_with = "lenient_text")]
    pub nick: Option<String>,
    /// A device's (`bluez5` is the one kept) and a node's device: the headset (quick Bluetooth
    /// spec § 4).
    #[serde(rename = "device.api", default, deserialize_with = "lenient_text")]
    pub device_api: Option<String>,
    #[serde(rename = "device.description", default, deserialize_with = "lenient_text")]
    pub device_description: Option<String>,
    #[serde(rename = "device.id", default, deserialize_with = "lenient_u32")]
    pub device_id: Option<u32>,
}

/// `Object`'s twin for a model built for privacy (privacy spec § 8): the same fields plus what the
/// capture rules read. Only `follow_tapped` with the tap's privacy flag deserializes it; without
/// the module the lean `Object` is parsed and nothing more.
#[derive(Deserialize)]
pub struct PrivacyObject {
    pub id: u32,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub info: Option<PrivacyInfo>,
    #[serde(default)]
    pub props: Option<MetaProps>,
    #[serde(default)]
    pub metadata: Option<Vec<MetaEntry>>,
}

#[derive(Deserialize, Default)]
pub struct PrivacyInfo {
    #[serde(default, deserialize_with = "null_as_default")]
    pub props: PrivacyProps,
    #[serde(default, deserialize_with = "null_as_default")]
    pub params: Params,
    /// A node's (`running`) or a link's (`active`) state; any other value, or a type that is not
    /// text, reads as neither (privacy spec § 2).
    #[serde(default, deserialize_with = "lenient_text")]
    pub state: Option<String>,
    /// A link's ends.
    #[serde(rename = "output-node-id", default, deserialize_with = "lenient_u32")]
    pub output_node: Option<u32>,
    #[serde(rename = "input-node-id", default, deserialize_with = "lenient_u32")]
    pub input_node: Option<u32>,
}

/// `NodeProps` and what the privacy rules read (spec § 2).
#[derive(Deserialize, Default)]
pub struct PrivacyProps {
    #[serde(rename = "media.class", default, deserialize_with = "lenient_text")]
    pub media_class: Option<String>,
    #[serde(rename = "node.name", default, deserialize_with = "lenient_text")]
    pub node_name: Option<String>,
    #[serde(rename = "node.description", default, deserialize_with = "lenient_text")]
    pub description: Option<String>,
    #[serde(rename = "node.nick", default, deserialize_with = "lenient_text")]
    pub nick: Option<String>,
    // What the privacy rules read (spec § 2); parsed always, kept only with the module.
    #[serde(rename = "media.role", default, deserialize_with = "lenient_text")]
    pub media_role: Option<String>,
    #[serde(rename = "device.api", default, deserialize_with = "lenient_text")]
    pub device_api: Option<String>,
    #[serde(rename = "application.name", default, deserialize_with = "lenient_text")]
    pub application_name: Option<String>,
    #[serde(rename = "application.process.binary", default, deserialize_with = "lenient_text")]
    pub process_binary: Option<String>,
    #[serde(rename = "pipewire.access.portal.app_id", default, deserialize_with = "lenient_text")]
    pub portal_app_id: Option<String>,
    #[serde(rename = "stream.monitor", default, deserialize_with = "lenient_text")]
    pub stream_monitor: Option<String>,
    #[serde(rename = "stream.capture.sink", default, deserialize_with = "lenient_text")]
    pub capture_sink: Option<String>,
    #[serde(rename = "node.passive", default, deserialize_with = "lenient_text")]
    pub passive: Option<String>,
    #[serde(rename = "node.link-group", default, deserialize_with = "lenient_text")]
    pub link_group: Option<String>,
    #[serde(rename = "device.description", default, deserialize_with = "lenient_text")]
    pub device_description: Option<String>,
    #[serde(rename = "device.id", default, deserialize_with = "lenient_u32")]
    pub device_id: Option<u32>,
}

#[derive(Deserialize, Default)]
pub struct Params {
    #[serde(rename = "Props", default, deserialize_with = "null_as_default")]
    pub props: Vec<PropsParam>,
    /// A device's profiles and its active one (the headset, quick Bluetooth spec § 4).
    #[serde(rename = "EnumProfile", default, deserialize_with = "null_as_default")]
    pub enum_profile: Vec<ProfileParam>,
    #[serde(rename = "Profile", default, deserialize_with = "null_as_default")]
    pub profile: Vec<ProfileParam>,
}

/// One profile of a device, every field lenient: a wrong type reads None, never fails the batch.
#[derive(Deserialize)]
pub struct ProfileParam {
    #[serde(default, deserialize_with = "lenient_u32")]
    pub index: Option<u32>,
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "lenient_text")]
    pub available: Option<String>,
}

#[derive(Deserialize)]
pub struct PropsParam {
    #[serde(rename = "channelVolumes")]
    pub channel_volumes: Option<Vec<f64>>,
    pub mute: Option<bool>,
}

#[derive(Deserialize)]
pub struct MetaProps {
    #[serde(rename = "metadata.name", default, deserialize_with = "lenient_text")]
    pub name: Option<String>,
}

/// One metadata entry. Everything optional: `{"subject":0,"key":null,…}` is
/// how a cleared subject is dumped, and an entry this module doesn't expect
/// must be skipped, not end the stream.
#[derive(Deserialize)]
pub struct MetaEntry {
    #[serde(default, deserialize_with = "lenient_u32")]
    pub subject: Option<u32>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: serde_json::Value,
}

/// A property pw-dump may print as a bare JSON number or boolean instead of
/// a string, when its value happens to look like one (the real dump has
/// `"alsa.card":3`, `"node.driver":true` next to genuine strings on the same
/// kind of object): kept as text; anything else (array, object, missing,
/// null) becomes `None` rather than failing the whole batch.
fn lenient_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    })
}

/// A field that should be its default value (an empty struct, an empty
/// vec…) when pw-dump writes it as JSON `null` instead of omitting it.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `subject` as a plain, non-negative id; a negative number or any other
/// JSON type becomes `None` (never matches a real object id) rather than
/// failing the whole batch.
fn lenient_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        _ => None,
    })
}

/// A priority as a plain number; any other JSON type becomes `None`.
fn lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        _ => None,
    })
}
