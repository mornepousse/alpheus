use super::*;
use crate::clock::LocalTime;
use crate::compositor::{CompositorState, Window};
use crate::config::{BarConfig, BatteryVisibility, ModuleId, ResourceItem};
use crate::modules::MODULES;
use crate::notify::NotifyView;
use crate::state::{Failed, Feed};
use crate::system::{
    Battery, BtDevice, BtState, Charge, GpuBusy, Link, NetRate, NetState, Peripheral, PeripheralKind, PowerState, Sample, SoundState,
    Volume,
};

fn bar() -> Bar {
    Bar::new(BarConfig::default())
}

fn peripheral(kind: PeripheralKind, name: &str, percent: Option<u8>, low: bool) -> Peripheral {
    Peripheral { kind, name: name.into(), percent, low }
}

#[test]
fn the_clock_tooltip_is_the_full_date() {
    let mut b = bar();
    assert_eq!(clock(&b), None, "no time yet");
    b.clock = Some(LocalTime { year: 2026, month: 11, day: 27, weekday: 5, hour: 9, minute: 3, second: 0 });
    assert_eq!(clock(&b).as_deref(), Some("Friday 27 November 2026"));
}

#[test]
fn the_volume_tooltip_says_percent_or_muted() {
    let mut b = bar();
    let sound = |percent, muted| Feed::Ready(SoundState { sink: Some(Volume { percent, muted }), ..SoundState::default() });
    b.sound = sound(45, false);
    assert_eq!(volume(&b).as_deref(), Some("Volume 45 %"));
    b.sound = sound(45, true);
    assert_eq!(volume(&b).as_deref(), Some("Volume muted (45 %)"));
    b.sound = Feed::Down;
    assert_eq!(volume(&b).as_deref(), Some("Volume unavailable"));
    b.sound = Feed::Ready(SoundState::default());
    assert_eq!(volume(&b), None, "no output: the bar draws nothing");
}

#[test]
fn the_network_tooltip_says_link_vpns_and_limits() {
    let mut b = bar();
    let online = |link, vpns, limited| Feed::Ready(NetState::Online { link, vpns, limited });
    b.network = online(Link::Wifi, 0, false);
    assert_eq!(network(&b).as_deref(), Some("Wi-Fi"));
    b.network = online(Link::Ethernet, 2, false);
    assert_eq!(network(&b).as_deref(), Some("Ethernet · 2 VPNs"));
    b.network = online(Link::Wifi, 1, false);
    assert_eq!(network(&b).as_deref(), Some("Wi-Fi · 1 VPN"));
    b.network = online(Link::Wifi, 0, true);
    assert_eq!(network(&b).as_deref(), Some("Wi-Fi · limited connectivity"));
    b.network = online(Link::Vpn, 0, false);
    assert_eq!(network(&b).as_deref(), Some("VPN"));
    b.network = Feed::Ready(NetState::Offline);
    assert_eq!(network(&b).as_deref(), Some("Offline"));
    b.network = Feed::Down;
    assert_eq!(network(&b).as_deref(), Some("Network unavailable"));
    b.network = Feed::Ready(NetState::Absent);
    assert_eq!(network(&b), None, "no NetworkManager: the bar draws nothing");
}

#[test]
fn the_battery_tooltip_names_kinds_never_devices() {
    let mut b = bar();
    b.battery = Feed::Ready(PowerState {
        laptop: Some(Battery { percent: 60, charge: Charge::Discharging }),
        peripherals: vec![peripheral(PeripheralKind::Mouse, "Kettle MX", Some(40), false)],
    });
    let text = battery(&b).expect("a tooltip");
    assert_eq!(text, "Battery 60 %, discharging · Mouse 40 %");
    assert!(!text.contains("Kettle"), "{text}");
    // A coarse-only peripheral follows `[battery] coarse` as the bar does: hidden at its default
    // unless low, shown (without a figure) when the setting shows it.
    b.battery = Feed::Ready(PowerState {
        laptop: Some(Battery { percent: 80, charge: Charge::Charging }),
        peripherals: vec![peripheral(PeripheralKind::Keyboard, "Kettle K2", None, false)],
    });
    assert_eq!(battery(&b).as_deref(), Some("Battery 80 %, charging"));
    b.config.battery.coarse = BatteryVisibility::Always;
    assert_eq!(battery(&b).as_deref(), Some("Battery 80 %, charging · Keyboard"));
    b.battery = Feed::Ready(PowerState {
        laptop: None,
        peripherals: vec![
            peripheral(PeripheralKind::Headset, "Kettle H", None, true),
            peripheral(PeripheralKind::Other, "Kettle", Some(9), false),
        ],
    });
    assert_eq!(battery(&b).as_deref(), Some("Headset low · Device 9 %"));
    b.config.battery.peripherals = BatteryVisibility::Never;
    b.config.battery.coarse = BatteryVisibility::Never;
    assert_eq!(battery(&b), None, "nothing drawn, nothing said");
    b.battery = Feed::Ready(PowerState { laptop: Some(Battery { percent: 100, charge: Charge::Full }), peripherals: vec![] });
    assert_eq!(battery(&b).as_deref(), Some("Battery 100 %, full"));
    b.battery = Feed::Ready(PowerState { laptop: Some(Battery { percent: 79, charge: Charge::PendingCharge }), peripherals: vec![] });
    assert_eq!(battery(&b).as_deref(), Some("Battery 79 %, plugged in"));
    b.battery = Feed::Ready(PowerState { laptop: Some(Battery { percent: 50, charge: Charge::Unknown }), peripherals: vec![] });
    assert_eq!(battery(&b).as_deref(), Some("Battery 50 %"));
    b.battery = Feed::Down;
    assert_eq!(battery(&b).as_deref(), Some("Battery unavailable"));
}

#[test]
fn the_bluetooth_tooltip_shows_the_drawn_device_whole() {
    let mut b = bar();
    let long = "Studio Headphones Pro Max Edition";
    b.bluetooth = Feed::Ready(BtState::On {
        devices: vec![
            BtDevice { name: long.into(), battery: Some(80) },
            BtDevice { name: "Second".into(), battery: None },
            BtDevice { name: "Third".into(), battery: None },
        ],
    });
    assert_eq!(bluetooth(&b).as_deref(), Some("Studio Headphones Pro Max Edition · 80 % · 2 more"), "whole, though the bar cuts it");
    b.bluetooth = Feed::Ready(BtState::On { devices: vec![BtDevice { name: long.into(), battery: None }] });
    assert_eq!(bluetooth(&b).as_deref(), Some(long));
    b.bluetooth = Feed::Ready(BtState::On { devices: vec![] });
    assert_eq!(bluetooth(&b).as_deref(), Some("Bluetooth on, nothing connected"));
    b.bluetooth = Feed::Ready(BtState::On { devices: vec![BtDevice { name: "\u{200b}".into(), battery: None }] });
    assert_eq!(bluetooth(&b).as_deref(), Some("Bluetooth connected"), "a name the bar draws as nothing");
    b.bluetooth = Feed::Ready(BtState::Off);
    assert_eq!(bluetooth(&b), None);
    b.bluetooth = Feed::Down;
    assert_eq!(bluetooth(&b).as_deref(), Some("Bluetooth unavailable"));
}

#[test]
fn the_resources_tooltip_follows_show() {
    let mut b = bar();
    assert_eq!(resources(&b), None, "no reading yet");
    b.resources.push(Sample {
        cpu: Some(12),
        memory: Some(61),
        temperature: Some(60),
        net: Some(NetRate { down: 2048, up: 0 }),
        disks: [Some(42), Some(70), None, None],
        gpu: Some(55),
        gpu_busy: Some(GpuBusy { percent: 40, idle_based: false }),
        vram: Some(30),
        memory_total: Some(156),
    });
    b.config.resources.show = vec![ResourceItem::Cpu, ResourceItem::Memory, ResourceItem::Temperature];
    assert_eq!(resources(&b).as_deref(), Some("CPU 12 % · Memory 6.1 GiB · Temperature 60 °C"));
    b.config.resources.show = vec![ResourceItem::Gpu, ResourceItem::Disk, ResourceItem::Net];
    assert_eq!(
        resources(&b).as_deref(),
        Some("GPU 40 %, 55 °C · Disk 70 % · Download 2.0K/s, upload 0B/s"),
        "in the order of show; the fullest disk and the GPU usage, as the bar draws them"
    );
    b.resources.push(Sample { gpu: Some(55), ..Sample::default() });
    b.config.resources.show = vec![ResourceItem::Gpu, ResourceItem::Disk];
    assert_eq!(resources(&b).as_deref(), Some("GPU 55 °C"), "no usage: the degrees alone, no disk figure");
    b.resources.push(Sample::default());
    b.config.resources.show = vec![ResourceItem::Cpu, ResourceItem::Memory];
    assert_eq!(resources(&b).as_deref(), Some("CPU …"), "the CPU before its first figure, as the bar draws it");
    b.config.resources.show = vec![ResourceItem::Memory];
    assert_eq!(resources(&b), None, "no figure, nothing said");
}

#[test]
fn the_window_tooltip_is_the_title_never_the_class() {
    let mut b = bar();
    let title = "A very long page title that the bar cuts — Browser";
    b.compositor = Feed::Ready(CompositorState {
        window: Some(Window { class: "secret-app-id".into(), title: title.into() }),
        ..CompositorState::default()
    });
    let text = window(&b).expect("a tooltip");
    assert_eq!(text, title);
    assert!(!text.contains("secret"), "{text}");
    b.compositor =
        Feed::Ready(CompositorState { window: Some(Window { class: "x".into(), title: String::new() }), ..CompositorState::default() });
    assert_eq!(window(&b), None, "no title, nothing said");
    b.compositor = Feed::Ready(CompositorState::default());
    assert_eq!(window(&b), None, "no focused window");
    b.compositor = Feed::Down;
    assert_eq!(window(&b).as_deref(), Some("Window unavailable"));
}

#[test]
fn the_failed_tooltip_counts_never_names() {
    let mut b = bar();
    assert_eq!(failed(&b), None, "neither count known");
    b.failed = Failed { system: Some(1), user: Some(1) };
    assert_eq!(failed(&b).as_deref(), Some("2 failed units (system 1, user 1)"));
    b.failed = Failed { system: Some(1), user: Some(0) };
    assert_eq!(failed(&b).as_deref(), Some("1 failed unit (system 1)"));
    b.failed = Failed { system: None, user: Some(3) };
    assert_eq!(failed(&b).as_deref(), Some("3 failed units (user 3)"));
    b.failed = Failed { system: Some(0), user: Some(0) };
    assert_eq!(failed(&b), None, "none failed: the bar draws nothing");
}

#[test]
fn the_bell_tooltip_counts_and_dnd() {
    let mut b = bar();
    assert_eq!(notifications(&b), None);
    b.notifications = Some(NotifyView { unread: 3, dnd: true, serving: true });
    assert_eq!(notifications(&b).as_deref(), Some("3 unread · do not disturb"));
    b.notifications = Some(NotifyView { unread: 1, dnd: false, serving: true });
    assert_eq!(notifications(&b).as_deref(), Some("1 unread"));
    b.notifications = Some(NotifyView { unread: 0, dnd: false, serving: true });
    assert_eq!(notifications(&b).as_deref(), Some("Nothing unread"));
    b.notifications = Some(NotifyView { unread: 120, dnd: false, serving: true });
    assert_eq!(notifications(&b).as_deref(), Some("120 unread"));
    b.notifications = Some(NotifyView { unread: 0, dnd: false, serving: false });
    assert_eq!(notifications(&b).as_deref(), Some("Another notification server is running"));
}

#[test]
fn waiting_feeds_have_no_tooltip() {
    let b = bar();
    for (name, f) in [
        ("clock", clock as fn(&Bar) -> Option<String>),
        ("battery", battery),
        ("volume", volume),
        ("network", network),
        ("bluetooth", bluetooth),
        ("resources", resources),
        ("window", window),
        ("failed", failed),
        ("notifications", notifications),
    ] {
        assert_eq!(f(&b), None, "{name} before its first reading");
    }
}

#[test]
fn only_nine_modules_have_a_tooltip() {
    let with: Vec<ModuleId> = MODULES.iter().filter(|s| s.tooltip.is_some()).map(|s| s.id).collect();
    assert_eq!(
        with,
        [
            ModuleId::Window,
            ModuleId::Failed,
            ModuleId::Volume,
            ModuleId::Network,
            ModuleId::Bluetooth,
            ModuleId::Battery,
            ModuleId::Resources,
            ModuleId::Notifications,
            ModuleId::Clock,
        ]
    );
    // Each row points at its own module's text, not a neighbour's.
    let mut b = bar();
    b.sound = Feed::Down;
    assert_eq!(ModuleId::Volume.spec().tooltip.and_then(|f| f(&b)).as_deref(), Some("Volume unavailable"));
    b.network = Feed::Down;
    assert_eq!(ModuleId::Network.spec().tooltip.and_then(|f| f(&b)).as_deref(), Some("Network unavailable"));
}
