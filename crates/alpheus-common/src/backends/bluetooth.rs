//! Bluetooth: bluetoothctl.
//!
//! Never:
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/compositors.md (what
//! `wm-set` and `shell-ctl` stand for).

use super::{BResult, run_ok, unparsable};
use crate::exec::Executor;
use std::sync::Arc;

const TOOL: &str = "bluetoothctl";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtStatus {
    pub powered: bool,
    pub connected: Vec<BtDevice>,
}

pub trait Bluetooth: Send + Sync {
    fn status(&self) -> BResult<BtStatus>;
    fn set_power(&self, on: bool) -> BResult<()>;
}

pub struct BluetoothCtl {
    exec: Arc<dyn Executor>,
}

impl BluetoothCtl {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        BluetoothCtl { exec }
    }
}

impl Bluetooth for BluetoothCtl {
    fn status(&self) -> BResult<BtStatus> {
        let powered = parse_show(&run_ok(&*self.exec, TOOL, &["show"])?)?;
        let connected = if powered { parse_devices(&run_ok(&*self.exec, TOOL, &["devices", "Connected"])?) } else { Vec::new() };
        Ok(BtStatus { powered, connected })
    }

    fn set_power(&self, on: bool) -> BResult<()> {
        run_ok(&*self.exec, TOOL, &["power", if on { "on" } else { "off" }]).map(|_| ())
    }
}

/// `Powered: yes|no` field of the default controller.
pub fn parse_show(out: &str) -> BResult<bool> {
    for line in out.lines() {
        if let Some(v) = line.trim().strip_prefix("Powered:") {
            return match v.trim() {
                "yes" => Ok(true),
                "no" => Ok(false),
                other => Err(unparsable(TOOL, format!("Powered `{other}`"))),
            };
        }
    }
    if out.contains("No default controller") {
        Err(unparsable(TOOL, "no Bluetooth controller"))
    } else {
        Err(unparsable(TOOL, "missing `Powered` field"))
    }
}

/// `Device <MAC> <name>` lines; others are ignored.
pub fn parse_devices(out: &str) -> Vec<BtDevice> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("Device "))
        .filter_map(|rest| rest.split_once(' '))
        .map(|(mac, name)| BtDevice { mac: mac.to_string(), name: name.trim().to_string() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;

    const SHOW: &str = include_str!("../../tests/fixtures/bluetoothctl-show.txt");
    const DEVICES: &str = include_str!("../../tests/fixtures/bluetoothctl-devices.txt");

    #[test]
    fn real_show() {
        assert_eq!(parse_show(SHOW), Ok(true));
        assert_eq!(parse_show("Controller x\n\tPowered: no\n"), Ok(false));
    }

    #[test]
    fn no_controller() {
        let e = parse_show("No default controller available\n").unwrap_err();
        assert!(e.to_string().contains("controller"), "{e}");
        assert!(parse_show("Controller x\n").is_err());
    }

    #[test]
    fn real_devices() {
        assert_eq!(parse_devices(DEVICES), vec![BtDevice { mac: "AA:BB:CC:DD:EE:01".into(), name: "CasqueBT".into() }]);
        assert!(parse_devices("Waiting to connect to bluetoothd...\n").is_empty());
    }

    #[test]
    fn powered_off_does_not_list_devices() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("bluetoothctl show", "Controller x\n\tPowered: no\n");
        let st = BluetoothCtl::new(f.clone()).status().unwrap();
        assert!(!st.powered);
        assert_eq!(f.calls(), vec!["bluetoothctl show"]);
    }

    #[test]
    fn exact_commands() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("bluetoothctl show", SHOW);
        f.ok("bluetoothctl devices Connected", DEVICES);
        f.ok("bluetoothctl power on", "Changing power on succeeded\n");
        let b = BluetoothCtl::new(f.clone());
        assert_eq!(b.status().unwrap().connected.len(), 1);
        b.set_power(true).unwrap();
    }
}
