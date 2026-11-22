//! What the sound thread shares with the loop: the model, behind a lock, and whether the quick
//! panel is open.

use super::*;

/// What the sound thread shares with the loop (quick spec § 5): its pw-dump model, so an opening
/// quick panel reads the sinks at once (pw-dump is silent at rest), and whether that panel is open,
/// so the thread publishes the sink list (`Scope::Panel`) only then. Locked once per pw-dump batch
/// and once per open or close; a poisoned lock is taken over, never a panic.
#[derive(Debug, Default)]
pub struct SoundTap(Mutex<Tapped>);

#[derive(Debug, Default)]
pub(super) struct Tapped {
    pub(super) model: Model,
    /// The `privacy` module is on: every model this tap builds keeps the capture graph.
    pub(super) privacy: bool,
    pub(super) panel: bool,
    /// The last state published: what a batch is compared with.
    pub(super) last: Option<SoundState>,
}

impl SoundTap {
    /// A tap whose model keeps the capture graph when `privacy` is on.
    pub fn new(privacy: bool) -> SoundTap {
        SoundTap(Mutex::new(Tapped { model: Model::new(privacy), privacy, ..Tapped::default() }))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Tapped> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The panel opened: the thread publishes the sink list from its next batch on. Returns the
    /// whole state as the panel reads it now (the sinks, and both defaults with their mute), so
    /// what the base scope never published (a microphone mute without the OSD) is never stale.
    pub fn open(&self) -> SoundState {
        let mut t = self.lock();
        t.panel = true;
        t.model.state_in(Scope::Panel)
    }

    /// The panel closed: back to the base scope; the list the thread kept goes.
    pub fn close(&self) {
        let mut t = self.lock();
        t.panel = false;
        if let Some(last) = t.last.as_mut() {
            last.sinks = None;
            last.headset = None;
        }
    }

    pub fn is_open(&self) -> bool {
        self.lock().panel
    }

    /// Whether the last publication kept is holding a sink list (tests).
    #[cfg(test)]
    pub(super) fn keeps_a_list(&self) -> bool {
        self.lock().last.as_ref().is_some_and(|l| l.sinks.is_some())
    }
}

/// Empties the tap's model and last state when dropped: a stream that ended, however (its end, a
/// parse error, a panic `supervise` catches), leaves nothing an opening panel could read through
/// the backoff — never a dead PipeWire's volumes or node ids.
pub(super) struct Forget<'a>(pub(super) &'a SoundTap);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        let mut t = self.0.lock();
        t.model = Model::new(t.privacy);
        t.last = None;
    }
}
