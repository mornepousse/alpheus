//! Network: NetworkManager via nmcli ("terse" mode -t).

use super::{BResult, run_ok, unparsable};
use crate::exec::Executor;
use std::sync::Arc;

const TOOL: &str = "nmcli";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    /// Readable label: Wi-Fi, Ethernet, WireGuard…
    pub kind: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetStatus {
    pub wifi_enabled: bool,
    pub active: Vec<Connection>,
}

pub trait Network: Send + Sync {
    fn status(&self) -> BResult<NetStatus>;
    fn set_wifi(&self, on: bool) -> BResult<()>;
}

pub struct NmCli {
    exec: Arc<dyn Executor>,
}

impl NmCli {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        NmCli { exec }
    }
}

impl Network for NmCli {
    fn status(&self) -> BResult<NetStatus> {
        let wifi_enabled = parse_radio(&run_ok(&*self.exec, TOOL, &["-t", "radio", "wifi"])?)?;
        let active = parse_active(&run_ok(&*self.exec, TOOL, &["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"])?)?;
        Ok(NetStatus { wifi_enabled, active })
    }

    fn set_wifi(&self, on: bool) -> BResult<()> {
        run_ok(&*self.exec, TOOL, &["radio", "wifi", if on { "on" } else { "off" }]).map(|_| ())
    }
}

/// Fields separated by ":"; a field's ":" and "\" are escaped with "\".
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    fields.last_mut().unwrap().push(n);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

pub fn parse_radio(out: &str) -> BResult<bool> {
    match out.trim() {
        "enabled" => Ok(true),
        "disabled" => Ok(false),
        other => Err(unparsable(TOOL, format!("radio state `{other}`"))),
    }
}

fn kind_label(t: &str) -> String {
    match t {
        "802-11-wireless" => "Wi-Fi",
        "802-3-ethernet" => "Ethernet",
        "wireguard" => "WireGuard",
        "vpn" => "VPN",
        "bluetooth" => "Bluetooth",
        other => other,
    }
    .to_string()
}

pub fn parse_active(out: &str) -> BResult<Vec<Connection>> {
    let mut v = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let f = split_terse(line);
        if f.len() != 3 {
            return Err(unparsable(TOOL, format!("{} fields instead of 3", f.len())));
        }
        if f[1] == "loopback" {
            continue;
        }
        v.push(Connection { name: f[0].clone(), kind: kind_label(&f[1]), device: f[2].clone() });
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;

    const ACTIVE: &str = include_str!("../../tests/fixtures/nmcli-active.txt");

    #[test]
    fn splits_with_escapes() {
        assert_eq!(split_terse(r"a\:b:c\\d:e"), vec!["a:b", r"c\d", "e"]);
        assert_eq!(split_terse("alone"), vec!["alone"]);
    }

    #[test]
    fn real_active_connections_without_loopback() {
        let c = parse_active(ACTIVE).unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!(c[0], Connection { name: "Wired connection 1".into(), kind: "Ethernet".into(), device: "enp0s0u1u2i10".into() });
        assert_eq!(c[1].kind, "Wi-Fi");
        assert_eq!(c[1].name, "MON_WIFI_1");
        assert_eq!(c[2].kind, "WireGuard");
    }

    #[test]
    fn badly_formatted_line() {
        assert!(parse_active("just:two\n").is_err());
    }

    #[test]
    fn radio() {
        assert_eq!(parse_radio("enabled\n"), Ok(true));
        assert_eq!(parse_radio("disabled\n"), Ok(false));
        assert!(parse_radio("activé\n").is_err());
    }

    #[test]
    fn exact_commands() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("nmcli -t radio wifi", "enabled\n");
        f.ok("nmcli -t -f NAME,TYPE,DEVICE connection show --active", ACTIVE);
        f.ok("nmcli radio wifi off", "");
        let n = NmCli::new(f.clone());
        let st = n.status().unwrap();
        assert!(st.wifi_enabled);
        assert_eq!(st.active.len(), 4);
        n.set_wifi(false).unwrap();
    }
}
