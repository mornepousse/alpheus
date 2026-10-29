//! Reading `pw-dump -m`'s output array by array into the model.

use super::*;

/// Reads `pw-dump -m` until its output ends, with nobody to open a panel (`Scope::Bar` or
/// `Scope::Osd` throughout).
pub fn follow(output: impl Read, scope: Scope, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    follow_tapped(output, scope, &SoundTap::default(), publish)
}

/// Reads `pw-dump -m` until its output ends. Each array is parsed on its own (StreamDeserializer
/// over a BufReader), without the lock; then, under it, applied to the tap's model and compared
/// with the last state published under the scope in effect (`base`, or `Scope::Panel` while the
/// panel is open). A new stream starts from a new model and keeps the panel's flag; the model goes
/// when the stream does (`Forget`).
pub fn follow_tapped(output: impl Read, base: Scope, tap: &SoundTap, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    let privacy = {
        let mut t = tap.lock();
        t.model = Model::new(t.privacy);
        t.last = None;
        t.privacy
    };
    let _forget = Forget(tap);
    // Without the module the lean `Object` is parsed: nothing more (privacy spec § 8).
    if privacy {
        follow_batches::<PrivacyObject>(output, base, tap, publish, Model::apply_privacy)
    } else {
        follow_batches::<Object>(output, base, tap, publish, Model::apply)
    }
}

pub(super) fn follow_batches<T: serde::de::DeserializeOwned>(
    output: impl Read,
    base: Scope,
    tap: &SoundTap,
    publish: &mut dyn FnMut(SoundState) -> bool,
    apply: fn(&mut Model, Vec<T>),
) -> io::Result<()> {
    for batch in serde_json::Deserializer::from_reader(io::BufReader::new(output)).into_iter::<Vec<T>>() {
        let batch = batch.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("pw-dump: {e}")))?;
        let state = {
            let mut t = tap.lock();
            let Tapped { model, panel, last, .. } = &mut *t;
            apply(model, batch);
            let scope = if *panel { Scope::Panel } else { base };
            if last.as_ref().is_some_and(|l| model.matches(l, scope)) {
                continue;
            }
            let s = model.state_in(scope);
            *last = Some(s.clone());
            s
        };
        if !publish(state) {
            return Ok(());
        }
    }
    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pw-dump: end of output"))
}
