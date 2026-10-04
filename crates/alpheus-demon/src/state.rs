//! The bar's pure state: what the sources have said, nothing else.
//! Messages change it; `layout` derives the grid for each screen from it.

use crate::clock::LocalTime;
use crate::compositor::CompositorState;
use crate::config::BarConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed<T> {
    /// Nothing received yet: "…".
    Waiting,
    /// Source down: "–".
    Down,
    Ready(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// None: the compositor source is down.
    Compositor(Option<CompositorState>),
    Clock(LocalTime),
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub config: BarConfig,
    pub compositor: Feed<CompositorState>,
    pub clock: Option<LocalTime>,
}

impl Bar {
    pub fn new(config: BarConfig) -> Bar {
        Bar { config, compositor: Feed::Waiting, clock: None }
    }

    /// Applies a message; true if the display may have changed.
    pub fn apply(&mut self, m: Msg) -> bool {
        match m {
            Msg::Compositor(e) => {
                let n = match e {
                    Some(e) => Feed::Ready(e),
                    None => Feed::Down,
                };
                if n == self.compositor {
                    false
                } else {
                    self.compositor = n;
                    true
                }
            }
            Msg::Clock(h) => {
                if self.clock == Some(h) {
                    false
                } else {
                    self.clock = Some(h);
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::example;

    const H: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    #[test]
    fn messages_and_changes() {
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.compositor, Feed::Waiting);
        assert!(b.apply(Msg::Compositor(Some(example()))));
        assert!(!b.apply(Msg::Compositor(Some(example()))), "same state: nothing to redraw");
        assert!(b.apply(Msg::Compositor(None)));
        assert_eq!(b.compositor, Feed::Down);
        assert!(!b.apply(Msg::Compositor(None)));
        assert!(b.apply(Msg::Clock(H)));
        assert!(!b.apply(Msg::Clock(H)));
        assert_eq!(b.clock, Some(H));
    }
}
