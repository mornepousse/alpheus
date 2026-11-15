//! Low-battery alerts: the laptop's percentage against `[battery] warn`, one notification per
//! threshold crossed while discharging, re-armed by any charge. Pure: `battery_feed`
//! (notify_app.rs) hands the result to the notification server.

use crate::config::ModuleId;
use crate::notify::{Incoming, Urgency};
use crate::system::{Battery, Charge};

/// One alert: the threshold crossed, the percentage seen, and whether it is the lowest threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub threshold: u8,
    pub percent: u8,
    pub critical: bool,
}

pub struct BatteryAlerts {
    /// Descending.
    thresholds: Vec<u8>,
    /// The lowest threshold alerted since the last charge.
    fired: Option<u8>,
    /// The latest reading, so an alert held back while the server was not serving can go out
    /// when it starts (`last`).
    last: Option<Battery>,
}

impl BatteryAlerts {
    pub fn new(warn: &[u8]) -> BatteryAlerts {
        let mut thresholds = warn.to_vec();
        thresholds.sort_unstable_by(|a, b| b.cmp(a));
        thresholds.dedup();
        BatteryAlerts { thresholds, fired: None, last: None }
    }

    /// The latest reading seen.
    pub fn last(&self) -> Option<Battery> {
        self.last
    }

    /// A reading of the laptop's battery. Discharging: the lowest threshold at or above the
    /// percentage and below the last one alerted gives the alert (a jump past two gives one).
    /// The alert counts as given only when `serving` (someone can show it): otherwise the state
    /// stays armed and `last` re-offers it later. Any charge re-arms; an unknown state or no
    /// battery changes nothing.
    pub fn feed(&mut self, laptop: Option<Battery>, serving: bool) -> Option<Alert> {
        let b = laptop?;
        self.last = Some(b);
        match b.charge {
            Charge::Charging | Charge::Full | Charge::PendingCharge => {
                self.fired = None;
                None
            }
            Charge::Unknown => None,
            Charge::Discharging => {
                let below = self.fired.unwrap_or(u8::MAX);
                let t = self.thresholds.iter().copied().filter(|&t| b.percent <= t && t < below).min()?;
                if !serving {
                    return None;
                }
                self.fired = Some(t);
                Some(Alert { threshold: t, percent: b.percent, critical: self.thresholds.last() == Some(&t) })
            }
        }
    }
}

/// The notification an alert becomes: sticky (no expiry), `device.warning`, critical at the lowest threshold.
pub fn incoming(a: Alert) -> Incoming {
    Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary: if a.critical { "Battery critical" } else { "Battery low" }.into(),
        body: format!("{} % left", a.percent),
        actions: Vec::new(),
        urgency: if a.critical { Urgency::Critical } else { Urgency::Normal },
        expire_ms: -1,
        desktop_entry: None,
        category: Some("device.warning".into()),
        transient: false,
        resident: false,
        tag: None,
    }
}

/// Alerts run with both the battery and the notifications modules, and a threshold to watch.
pub fn alerts_wanted(modules: &[ModuleId], warn: &[u8]) -> bool {
    modules.contains(&ModuleId::Battery) && modules.contains(&ModuleId::Notifications) && !warn.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: u8, charge: Charge) -> Option<Battery> {
        Some(Battery { percent: p, charge })
    }
    fn d(p: u8) -> Option<Battery> {
        at(p, Charge::Discharging)
    }
    fn c(p: u8) -> Option<Battery> {
        at(p, Charge::Charging)
    }
    fn alert(threshold: u8, percent: u8, critical: bool) -> Option<Alert> {
        Some(Alert { threshold, percent, critical })
    }

    #[test]
    fn crossing_each_threshold_alerts_once() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(25), true), None);
        assert_eq!(a.feed(d(20), true), alert(20, 20, false));
        assert_eq!(a.feed(d(19), true), None);
        assert_eq!(a.feed(d(10), true), alert(10, 10, false));
        assert_eq!(a.feed(d(9), true), None);
        assert_eq!(a.feed(d(5), true), alert(5, 5, true));
        assert_eq!(a.feed(d(4), true), None);
    }

    #[test]
    fn a_start_below_alerts_once_for_the_lowest_crossed() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(8), true), alert(10, 8, false));
        assert_eq!(a.feed(d(7), true), None);
        assert_eq!(a.feed(d(5), true), alert(5, 5, true));
    }

    #[test]
    fn a_jump_past_two_thresholds_alerts_once() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(25), true), None);
        assert_eq!(a.feed(d(9), true), alert(10, 9, false));
        assert_eq!(a.feed(d(9), true), None);
    }

    #[test]
    fn jitter_around_a_threshold_alerts_once() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(20), true), alert(20, 20, false));
        assert_eq!(a.feed(d(21), true), None);
        assert_eq!(a.feed(d(20), true), None);
    }

    #[test]
    fn charging_or_full_rearms() {
        for charge in [Charge::Charging, Charge::Full, Charge::PendingCharge] {
            let mut a = BatteryAlerts::new(&[20, 10, 5]);
            assert_eq!(a.feed(d(20), true), alert(20, 20, false));
            assert_eq!(a.feed(at(15, charge), true), None);
            assert_eq!(a.feed(d(15), true), alert(20, 15, false), "{charge:?}");
        }
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(20), true), alert(20, 20, false));
        assert_eq!(a.feed(c(15), true), None);
        assert_eq!(a.feed(d(15), true), alert(20, 15, false), "a plain charge re-arms");
        assert_eq!(a.feed(at(15, Charge::Unknown), true), None);
        assert_eq!(a.feed(d(15), true), None, "unknown re-arms nothing");
    }

    #[test]
    fn an_alert_while_not_serving_waits_for_the_server() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(d(8), false), None);
        assert_eq!(a.last(), d(8));
        assert_eq!(a.feed(a.last(), true), alert(10, 8, false), "served: exactly one, for 10 %");
        assert_eq!(a.feed(d(8), true), None);
        assert_eq!(a.feed(a.last(), true), None);
    }

    #[test]
    fn no_battery_or_no_threshold_never_alerts() {
        let mut a = BatteryAlerts::new(&[20, 10, 5]);
        assert_eq!(a.feed(None, true), None);
        let mut a = BatteryAlerts::new(&[]);
        assert_eq!(a.feed(d(1), true), None);
    }

    #[test]
    fn the_alert_texts() {
        let n = incoming(Alert { threshold: 20, percent: 19, critical: false });
        assert_eq!(
            (n.app_name.as_str(), n.summary.as_str(), n.body.as_str(), n.urgency),
            ("alpheus", "Battery low", "19 % left", Urgency::Normal)
        );
        assert_eq!((n.expire_ms, n.category.as_deref(), n.replaces_id), (-1, Some("device.warning"), 0));
        let n = incoming(Alert { threshold: 5, percent: 5, critical: true });
        assert_eq!((n.summary.as_str(), n.urgency), ("Battery critical", Urgency::Critical));
    }

    #[test]
    fn alerts_need_both_modules_and_a_threshold() {
        use ModuleId::{Battery, Clock, Notifications};
        assert!(alerts_wanted(&[Battery, Notifications, Clock], &[20]));
        assert!(!alerts_wanted(&[Battery, Clock], &[20]));
        assert!(!alerts_wanted(&[Notifications, Clock], &[20]));
        assert!(!alerts_wanted(&[Battery, Notifications], &[]));
    }
}
