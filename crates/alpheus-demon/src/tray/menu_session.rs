//! One open `com.canonical.dbusmenu` menu, the D-Bus side only, pure over
//! `Outbox`: `AboutToShow(0)` first (Electron builds its menu then), then
//! `GetLayout(0)` one level deep; a submenu is fetched when it opens; the
//! menu's signals (`LayoutUpdated`, `ItemsPropertiesUpdated`) patch it in
//! place while it is open — their match rule exists only that long; a
//! choice is `Event(id, "clicked")` with no reply expected; any call left
//! unanswered 2 s closes the menu. Drawing and keys live elsewhere.

use super::bus::{Call, CallError, CallId, Outbox};
use super::codec::{Layout, MenuNode, Reply, Signal};
use super::{ItemKey, MENU_INTERFACE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuUpdate {
    Nothing,
    Changed,
    Close,
}

/// The rule that lets one menu's signals through: sender, path, MENU_INTERFACE.
pub fn menu_rule(owner: &str, path: &str) -> String {
    format!("type='signal',sender='{owner}',path='{path}',interface='{MENU_INTERFACE}'")
}

/// What one of the session's calls was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Rule,
    AboutToShow(i32),
    Layout(i32),
}

pub struct MenuSession {
    pub key: ItemKey,
    pub path: String,
    layout: Option<Layout>,
    rule: String,
    pending: Vec<(CallId, Pending)>,
    /// Submenus asked for and not arrived yet.
    fetching: Vec<i32>,
    /// Submenus whose children arrived: a re-read of one of their ancestors is one level deep
    /// (graft carries their old children over, task 2), so they are fetched again to refresh them.
    fetched: Vec<i32>,
    /// Parents a `LayoutUpdated` named since the last frame: re-read once per frame (`on_frame`),
    /// however many signals a storm sends, like the host's dirty item properties.
    dirty: Vec<i32>,
}

impl MenuSession {
    /// AddMatch(menu_rule), then AboutToShow(0); GetLayout(0) follows its answer (or its error).
    pub fn open(key: ItemKey, path: String, out: &mut dyn Outbox) -> MenuSession {
        let rule = menu_rule(&key.owner, &path);
        let mut s = MenuSession {
            key,
            path,
            layout: None,
            rule: rule.clone(),
            pending: Vec::new(),
            fetching: Vec::new(),
            fetched: Vec::new(),
            dirty: Vec::new(),
        };
        s.call(Call::AddMatch { rule }, Pending::Rule, out);
        s.about_to_show(0, out);
        s
    }

    fn call(&mut self, call: Call, pending: Pending, out: &mut dyn Outbox) {
        let id = out.call(call);
        self.pending.push((id, pending));
    }

    fn asking(&self, p: Pending) -> bool {
        self.pending.iter().any(|(_, q)| *q == p)
    }

    fn about_to_show(&mut self, id: i32, out: &mut dyn Outbox) {
        let call = Call::AboutToShow { destination: self.key.owner.clone(), path: self.path.clone(), id };
        self.call(call, Pending::AboutToShow(id), out);
    }

    fn get_layout(&mut self, parent: i32, out: &mut dyn Outbox) {
        if !self.asking(Pending::Layout(parent)) {
            let call = Call::GetLayout { destination: self.key.owner.clone(), path: self.path.clone(), parent };
            self.call(call, Pending::Layout(parent), out);
        }
    }

    pub fn owns(&self, id: CallId) -> bool {
        self.pending.iter().any(|(c, _)| *c == id)
    }

    /// Any timeout → Close (spec: no answer within 2 s closes the menu). A GetLayout answer is the
    /// layout (root) or grafted (submenu) → Changed.
    pub fn answer(&mut self, id: CallId, result: Result<Reply, CallError>, out: &mut dyn Outbox) -> MenuUpdate {
        let Some(i) = self.pending.iter().position(|(c, _)| *c == id) else { return MenuUpdate::Nothing };
        let (_, pending) = self.pending.remove(i);
        if result == Err(CallError::Timeout) {
            return MenuUpdate::Close;
        }
        match pending {
            Pending::Rule => MenuUpdate::Nothing,
            // Many applications have no AboutToShow: its error still lets the layout be read.
            Pending::AboutToShow(parent) => {
                let update = result == Ok(Reply::NeedUpdate(true));
                let known = self.layout.as_ref().and_then(|l| l.node(parent)).is_some_and(|n| !n.children.is_empty());
                if parent == 0 || update || !known {
                    self.get_layout(parent, out);
                } else {
                    self.fetching.retain(|f| *f != parent);
                }
                MenuUpdate::Nothing
            }
            Pending::Layout(parent) => {
                self.fetching.retain(|f| *f != parent);
                // GetLayout(parent) must echo parent as the returned root's id; an application that
                // answers with a different node is bad, never grafted (it would otherwise graft onto
                // whatever other node in the tree happens to share that id).
                let result = match result {
                    Ok(Reply::Layout(l)) if l.root.id != parent => {
                        Err(CallError::BadReply(format!("GetLayout({parent}): root id {} returned", l.root.id)))
                    }
                    other => other,
                };
                match (result, &mut self.layout) {
                    (Ok(Reply::Layout(l)), None) if parent == 0 => {
                        self.layout = Some(l);
                        MenuUpdate::Changed
                    }
                    (Ok(Reply::Layout(l)), Some(layout)) => {
                        if !layout.graft(l.root) {
                            return MenuUpdate::Nothing;
                        }
                        // The revision only moves once the graft actually lands: on failure the tree
                        // is unchanged, and a bumped revision alone would hide the next real
                        // LayoutUpdated (its revision would already look "seen").
                        layout.revision = l.revision;
                        if parent != 0 && !self.fetched.contains(&parent) {
                            self.fetched.push(parent);
                        }
                        self.refetch_under(parent, out);
                        MenuUpdate::Changed
                    }
                    // A submenu reply arriving before the root is known: there is no tree to graft
                    // onto yet, but the root fetch already in flight (from `open`) will bring one —
                    // ignored, not a reason to close (the menu may still open fine).
                    (_, None) if parent != 0 => MenuUpdate::Nothing,
                    // No root layout at all: there is no menu to show.
                    (_, None) => MenuUpdate::Close,
                    _ => MenuUpdate::Nothing,
                }
            }
        }
    }

    /// After `parent` was grafted: the submenus fetched before under it (kept on screen by graft's
    /// carry-over, task 2) are read again (strictly below `parent`, so this ends); those gone from the menu are forgotten, unless a
    /// re-read in flight may bring them back.
    fn refetch_under(&mut self, parent: i32, out: &mut dyn Outbox) {
        let Some(layout) = &self.layout else { return };
        let Some(top) = layout.node(parent) else { return };
        let again: Vec<i32> = self.fetched.iter().copied().filter(|id| below(top, *id).is_some_and(|n| n.has_submenu())).collect();
        let rereading = !again.is_empty() || self.pending.iter().any(|(_, p)| matches!(p, Pending::Layout(_)));
        self.fetched.retain(|id| rereading || layout.node(*id).is_some());
        for id in again {
            self.get_layout(id, out);
        }
    }

    /// Signals of this menu only (sender and path checked): LayoutUpdated → parent marked dirty, then re-read (`on_frame`) and
    /// grafted (the root, or a submenu whose children are known; the submenus fetched below it are
    /// fetched again); ItemsPropertiesUpdated → patched in place (Changed when it names an entry of this menu).
    pub fn signal(&mut self, s: &Signal) -> MenuUpdate {
        match s {
            Signal::LayoutUpdated { sender, path, parent, revision } if self.mine(sender, path) => {
                // Before the first layout, the GetLayout(0) on its way reads the new one anyway; a submenu
                // never opened (no children known) is read when it opens.
                let known = self.layout.as_ref().and_then(|l| l.node(*parent)).is_some_and(|n| *parent == 0 || !n.children.is_empty());
                // Some applications always send revision 0 (no versioning on their side): a 0 is never
                // trusted as "already seen", or their second real change would be swallowed forever.
                // Prior art (libdbusmenu-glib's `dbusmenu-client.c`, `layout_update`) only refetches when
                // `revision > held`, which is exactly that trap for a 0-only sender; `!=` plus the 0
                // carve-out stays safe for both well- and badly-behaved applications.
                let stale = self.layout.as_ref().is_none_or(|l| *revision == 0 || *revision != l.revision);
                if known && stale && !self.dirty.contains(parent) {
                    self.dirty.push(*parent);
                }
                MenuUpdate::Nothing
            }
            Signal::ItemsPropertiesUpdated { sender, path, updated, removed } if self.mine(sender, path) => match &mut self.layout {
                // Changed only when one of the entries named is in this menu.
                Some(l) if updated.iter().map(|(id, _)| id).chain(removed.iter().map(|(id, _)| id)).any(|id| l.node(*id).is_some()) => {
                    l.patch(updated, removed);
                    MenuUpdate::Changed
                }
                Some(_) => MenuUpdate::Nothing,
                None => MenuUpdate::Nothing,
            },
            _ => MenuUpdate::Nothing,
        }
    }

    /// A `LayoutUpdated` is waiting for its re-read: the wiring asks for one frame.
    pub fn wants_frame(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// The frame callback: one GetLayout per dirty parent (none while one for it is in flight).
    pub fn on_frame(&mut self, out: &mut dyn Outbox) {
        for parent in std::mem::take(&mut self.dirty) {
            self.get_layout(parent, out);
        }
    }

    fn mine(&self, sender: &str, path: &str) -> bool {
        sender == self.key.owner && path == self.path
    }

    /// A submenu is being opened: AboutToShow(id), then GetLayout(id) when its children are not known yet.
    pub fn fetch_submenu(&mut self, id: i32, out: &mut dyn Outbox) {
        if self.fetching.contains(&id) {
            return;
        }
        self.fetching.push(id);
        self.about_to_show(id, out);
    }

    /// Event(id, "clicked", v(i32 0), timestamp), no reply expected.
    pub fn click(&mut self, id: i32, timestamp: u32, out: &mut dyn Outbox) {
        out.send(Call::Event { destination: self.key.owner.clone(), path: self.path.clone(), id, timestamp });
    }

    /// RemoveMatch; everything else dropped with self.
    pub fn close(self, out: &mut dyn Outbox) {
        out.send(Call::RemoveMatch { rule: self.rule });
    }

    pub fn layout(&self) -> Option<&Layout> {
        self.layout.as_ref()
    }
}

/// The node `id` strictly below `top`.
fn below(top: &MenuNode, id: i32) -> Option<&MenuNode> {
    top.children.iter().find_map(|c| if c.id == id { Some(c) } else { below(c, id) })
}

#[cfg(test)]
mod tests;
