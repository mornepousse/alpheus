//! Resources, read from /proc and /sys with no command at all, on the event
//! loop (a few µs per reading, every `[resources] interval` seconds): CPU use
//! since the previous reading (the aggregate `cpu` line only, so CPU hotplug
//! never confuses it), memory in use like htop (tenths of GiB, caches left
//! out, shared memory kept; see `parse_meminfo`), the chosen sensor's
//! temperature — the max across every match when several packages or
//! drivers share the hint's name, re-resolved by name whenever a cached
//! sensor file stops existing (hwmon numbers are not stable across a reboot
//! or a hot-(re)plug: never kept as a bare `hwmonN`), and the disks — each
//! `disk_mounts` path, else `$HOME` — by `statvfs` once every `disk_interval`
//! seconds, the last figures repeated in between (a `disk_mounts` path on a
//! network filesystem, found once at start in /proc/self/mountinfo, is never
//! read and shows —; a hung `$HOME` mount would still stall that call:
//! docs/decisions.md, "Disks are read every `disk_interval`
//! seconds, the last figures repeat between reads"), and the GPU's usage and
//! VRAM from its card's sysfs files (amdgpu's busy share, or Intel's estimate
//! from idle residency; the card picked by its driver link, `gpu_metric` deciding at start what is read; docs/decisions.md, "GPU
//! usage comes from sysfs only; NVIDIA proprietary shows —"). A file that
//! can't be read leaves its own figure out; the next tick tries again, the
//! next `disk_interval` for a disk (resources have no down state). `root` is
//! "/" in the daemon, a temporary tree in tests.
//!
//! Never:
//! - run a command: everything comes from `/proc` and `/sys`, which is why the resources module
//!   adds no process (`the_only_stream_is_pw_dump_monitor` pins the one child the daemon keeps,
//!   `the_tray_spawns_no_thread` lists this file);
//! - read what no item asks for: an item left out of `[resources] show` is never read, and
//!   `gpu_sensor` exists so that a discrete GPU's sensor is not woken on a hybrid laptop;
//! - wake a GPU or keep it from falling asleep: `device/power/runtime_status` is read before any
//!   usage file and a card that is not awake gives no figure (`only_an_active_card_is_read`); the
//!   usage file (and the card's hwmon in `both`) is read at most once per its autosuspend delay and
//!   a second (`usage_is_read_once_per_autosuspend_delay_and_a_second`);
//!   `auto` reads only the `boot_vga` card (docs/decisions.md, "A GPU's usage is read without
//!   keeping it awake");
//! - log a disk path, an interface or a card
//!   (`the_resources_reader_never_logs_a_mount_an_interface_or_a_card`);
//! - keep more than the ring: the last 60 samples, about 3.3 KB (`the_history_stays_small`).
//!
//! See: docs/features/resources.md (the items, the ring and the graphs), docs/configuration.md
//! (`[resources]`), docs/measurements.md (what a reading costs on the loop).

use crate::config::{GpuMetric, ResourceItem, ResourcesConfig};
use crate::log::write as log;
use crate::sys::FsBlocks;
use crate::system::{DISKS, GpuBusy, NetRate, Sample};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// The aggregate line of /proc/stat: `cpu  user nice system idle iowait irq softirq steal …`.
pub fn parse_stat(line: &str) -> Option<CpuTimes> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let v = fields.take(8).map(|x| x.parse::<u64>().ok()).collect::<Option<Vec<u64>>>()?;
    if v.len() < 5 {
        return None;
    }
    let total = v.iter().sum::<u64>();
    Some(CpuTimes { busy: total.saturating_sub(v[3] + v[4]), total })
}

/// CPU use between two readings, rounded; None if the counters didn't move forward.
pub fn cpu_percent(a: CpuTimes, b: CpuTimes) -> Option<u8> {
    let dt = b.total.checked_sub(a.total).filter(|d| *d > 0)?;
    let db = b.busy.checked_sub(a.busy)?;
    Some(((db * 100 + dt / 2) / dt).min(100) as u8)
}

/// KiB in a GiB.
const GIB_KB: u64 = 1024 * 1024;

/// Memory in use, in tenths of GiB, rounded (shown as "10.5G"). Like htop
/// (2026-10-06: RAM only, caches left out): MemTotal − MemFree −
/// Buffers − (Cached + SReclaimable − Shmem). Shmem (tmpfs, shared buffers)
/// sits inside Cached but cannot be reclaimed, so it stays counted as used;
/// SReclaimable or Shmem absent (old kernels) count as 0. Without Buffers or
/// Cached, MemTotal − MemAvailable, then MemTotal − MemFree (pre-3.14).
pub fn parse_meminfo(text: &str) -> Option<u16> {
    let field = |k: &str| field(text, k);
    let total = field("MemTotal:").filter(|t| *t > 0)?;
    let free_style = || {
        let reclaimable = (field("Cached:")? + field("SReclaimable:").unwrap_or(0)).saturating_sub(field("Shmem:").unwrap_or(0));
        Some(field("MemFree:")? + field("Buffers:")? + reclaimable)
    };
    let available = free_style().or_else(|| field("MemAvailable:")).or_else(|| field("MemFree:"))?;
    let used = total.saturating_sub(available);
    u16::try_from((used * 10 + GIB_KB / 2) / GIB_KB).ok()
}

/// The kB figure of a `/proc/meminfo` line.
fn field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix(key)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok())
}

/// Installed memory, in tenths of GiB, rounded.
pub fn parse_mem_total(text: &str) -> Option<u16> {
    let total = field(text, "MemTotal:").filter(|t| *t > 0)?;
    u16::try_from((total * 10 + GIB_KB / 2) / GIB_KB).ok()
}

fn first_line(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?).read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

/// A sysfs temperature (millidegrees) in °C, rounded.
pub fn read_temperature(path: &Path) -> Option<i16> {
    let milli: i64 = first_line(path)?.parse().ok()?;
    i16::try_from((milli as f64 / 1000.0).round() as i64).ok()
}

/// hwmon drivers of a CPU, by preference: Intel's package, AMD's Tdie/Tctl…
const HWMON: [&str; 4] = ["coretemp", "k10temp", "zenpower", "cpu_thermal"];
/// Thermal zones of a CPU, by preference.
const ZONES: [&str; 5] = ["x86_pkg_temp", "TCPU", "cpu-thermal", "cpu_thermal", "acpitz"];

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Within one hwmon directory, the file to read: the input labelled `Tdie`
/// when present (AMD, modern `k10temp`), else the one labelled `Tctl` (AMD,
/// `temp1_input` without a `Tdie`: offset and hotter than the die), else
/// `temp1_input` (Intel's `coretemp` package, and every driver with no
/// Tdie/Tctl distinction).
fn hwmon_temp(dir: &Path) -> PathBuf {
    let labelled = |label: &str| {
        entries(dir).into_iter().find_map(|p| {
            let stem = p.file_name()?.to_str()?.strip_suffix("_label")?.to_string();
            (first_line(&p)? == label).then(|| dir.join(format!("{stem}_input")))
        })
    };
    labelled("Tdie").or_else(|| labelled("Tctl")).unwrap_or_else(|| dir.join("temp1_input"))
}

/// Every hwmon directory with a temperature file: its driver name and that file.
fn hwmon_list(root: &Path) -> Vec<(String, PathBuf)> {
    entries(&root.join("sys/class/hwmon"))
        .into_iter()
        .filter_map(|d| Some((first_line(&d.join("name"))?, hwmon_temp(&d))))
        .filter(|(_, p)| p.exists())
        .collect()
}

/// hwmon drivers of a GPU, by preference.
const GPU_HWMON: [&str; 5] = ["amdgpu", "nouveau", "i915", "xe", "nvidia"];

/// Every hwmon temperature file of the GPU named `hint`, or, empty, of the first driver of
/// `GPU_HWMON` found. Never a thermal zone (none names a GPU reliably); several matches when
/// several cards share a driver, the caller takes the max.
pub fn pick_gpu(root: &Path, hint: &str) -> Vec<PathBuf> {
    let hwmon = hwmon_list(root);
    let find_all = |name: &str| -> Vec<PathBuf> { hwmon.iter().filter(|(n, _)| n == name).map(|(_, p)| p.clone()).collect() };
    if !hint.is_empty() {
        return find_all(hint);
    }
    GPU_HWMON.iter().map(|n| find_all(n)).find(|v| !v.is_empty()).unwrap_or_default()
}

/// Every hwmon or thermal-zone file matching `hint` (or, empty, the first
/// name found by preference) — several when more than one package or driver
/// shares that name; the caller takes the max.
pub fn pick_sensor(root: &Path, hint: &str) -> Vec<PathBuf> {
    let hwmon = hwmon_list(root);
    let zones: Vec<(String, PathBuf)> = entries(&root.join("sys/class/thermal"))
        .into_iter()
        .filter(|d| d.file_name().is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone")))
        .filter_map(|d| Some((first_line(&d.join("type"))?, d.join("temp"))))
        .collect();
    let find_all = |list: &[(String, PathBuf)], name: &str| -> Vec<PathBuf> {
        list.iter().filter(|(n, _)| n == name).map(|(_, p)| p.clone()).collect()
    };
    if !hint.is_empty() {
        let hw = find_all(&hwmon, hint);
        return if !hw.is_empty() { hw } else { find_all(&zones, hint) };
    }
    for n in HWMON {
        let hw = find_all(&hwmon, n);
        if !hw.is_empty() {
            return hw;
        }
    }
    for n in ZONES {
        let z = find_all(&zones, n);
        if !z.is_empty() {
            return z;
        }
    }
    Vec::new()
}

/// While the sensor list is empty (nothing matched `hint` yet, or no sensor
/// at all), how many ticks to wait before scanning hwmon/thermal again:
/// re-running `pick_sensor` walks both directories and reads a `name`/`type`
/// file per entry, real work compared to the few `canonicalize` calls a
/// non-empty list costs every tick — cheap enough to afford once a tick, but
/// pointless to repeat that often for a sensor that may never appear (a
/// wrong `hint`, hardware without one).
const RETRY_EMPTY_EVERY_TICKS: u32 = 30;

/// `statvfs(3)`, injectable: tests never ask the real filesystem.
pub type StatFs = fn(&Path) -> Option<FsBlocks>;

/// Where the reader looks: `root` is "/" in the daemon and a temporary tree in tests, `home` the
/// directory whose filesystem the disk figure measures (None: no figure), `statfs` the call.
pub struct Roots {
    pub root: PathBuf,
    pub home: Option<PathBuf>,
    pub statfs: StatFs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetCounters {
    pub rx: u64,
    pub tx: u64,
}

/// The interface of the IPv4 default route in /proc/net/route text: among the lines that are up
/// (flag 1) with a zero destination and mask, the lowest metric (the first on a tie).
pub fn default_route(route: &str) -> Option<String> {
    route
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (iface, dest, _gateway, flags) = (f.next()?, f.next()?, f.next()?, f.next()?);
            let metric = f.nth(2)?.parse::<u32>().ok()?;
            let mask = f.next()?;
            let up = u32::from_str_radix(flags, 16).ok()? & 1 != 0;
            (up && dest == "00000000" && mask == "00000000").then_some((metric, iface))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, iface)| iface.to_string())
}

/// Bytes received and sent by `iface` in /proc/net/dev text (the exact name, not a prefix).
pub fn net_counters(dev: &str, iface: &str) -> Option<NetCounters> {
    let line = dev.lines().find_map(|l| l.trim_start().strip_prefix(iface)?.strip_prefix(':'))?;
    let mut f = line.split_whitespace();
    let rx = f.next()?.parse().ok()?;
    let tx = f.nth(7)?.parse().ok()?;
    Some(NetCounters { rx, tx })
}

/// Bytes per second between two readings; None for counters that went back (an interface reset)
/// or no time elapsed.
pub fn net_rate(prev: NetCounters, now: NetCounters, elapsed: Duration) -> Option<NetRate> {
    let ms = Some(elapsed.as_millis()).filter(|m| *m > 0)?;
    let per_s = |a: u64, b: u64| Some(u64::try_from(u128::from(b.checked_sub(a)?) * 1000 / ms).unwrap_or(u64::MAX));
    Some(NetRate { down: per_s(prev.rx, now.rx)?, up: per_s(prev.tx, now.tx)? })
}

/// Disk in use like `df`: used / (used + available), rounded up, so a nearly full disk never reads
/// lower than `df`'s. None for a filesystem with no blocks. Computed in u128: no overflow.
pub fn disk_percent(fs: FsBlocks) -> Option<u8> {
    let used = u128::from(fs.blocks.saturating_sub(fs.free));
    let denom = used + u128::from(fs.avail);
    if fs.blocks == 0 || denom == 0 {
        return None;
    }
    Some(((used * 100).div_ceil(denom)).min(100) as u8)
}

/// Filesystem types a `statvfs` could block on: served over the network, or an automount point
/// (`autofs`), which a `statvfs` would mount there and then (a NAS share, on the loop). A
/// `disk_mounts` path on one is never `statvfs`-ed, so a server that stops answering can never
/// block the loop (docs/decisions.md, "Disks are read every `disk_interval` seconds, the last
/// figures repeat between reads"). A local automount (systemd's `/boot`) shows — as well.
const NETWORK_FS: [&str; 14] =
    ["nfs", "nfs4", "cifs", "smb3", "smbfs", "sshfs", "9p", "ceph", "glusterfs", "davfs", "afs", "lustre", "autofs", "fuse"];

/// FUSE filesystems known to be local: every other `fuse.*` type (sshfs, rclone, gvfs…) counts as
/// remote, as an unknown one could be served over the network.
const LOCAL_FUSE: [&str; 3] = ["fuse.gocryptfs", "fuse.cryfs", "fuse.encfs"];

/// Whether a `statvfs` on a filesystem of type `fstype` (mountinfo's field after ` - `) could block.
fn remote_fs(fstype: &str) -> bool {
    NETWORK_FS.contains(&fstype) || (fstype.starts_with("fuse.") && !LOCAL_FUSE.contains(&fstype))
}

/// A mount point of `/proc/self/mountinfo`, its `\NNN` octal escapes (a space is `\040`) undone.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b.get(i + 1..i + 4).filter(|d| b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match octal {
            Some(d) => {
                out.push(d.iter().fold(0u8, |n, c| n.wrapping_mul(8).wrapping_add(c - b'0')));
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Whether `path` lies on a network filesystem, per `mountinfo` (the text of /proc/self/mountinfo):
/// the mount with the longest mount point containing `path`, the later line on a tie (a mount over
/// another). Matched by components, with no access to `path` itself (resolving it could hang on
/// the very mount it is about): a symlink to a network mount is not followed.
pub fn on_network_fs(mountinfo: &str, path: &Path) -> bool {
    let mut best: Option<(usize, bool)> = None;
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        let Some(point) = fields.nth(4).map(unescape) else { continue };
        let Some(fstype) = line.split_once(" - ").and_then(|(_, rest)| rest.split(' ').next()) else { continue };
        let depth = point.components().count();
        if path.starts_with(&point) && best.is_none_or(|(d, _)| depth >= d) {
            best = Some((depth, remote_fs(fstype)));
        }
    }
    best.is_some_and(|(_, remote)| remote)
}

/// The drivers whose cards offer a usage figure without root, in the order the automatic pick
/// prefers them; every other driver (evdi's monitor-only cards, NVIDIA's proprietary one) never wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDriver {
    Amdgpu,
    I915,
    Xe,
}

impl GpuDriver {
    const PREFERENCE: [GpuDriver; 3] = [GpuDriver::Amdgpu, GpuDriver::I915, GpuDriver::Xe];

    fn from_name(name: &str) -> Option<GpuDriver> {
        match name {
            "amdgpu" => Some(GpuDriver::Amdgpu),
            "i915" => Some(GpuDriver::I915),
            "xe" => Some(GpuDriver::Xe),
            _ => None,
        }
    }
}

/// A GPU read for usage: `dir` is its `sys/class/drm/card<N>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub dir: PathBuf,
    pub driver: GpuDriver,
}

/// The card to read for usage among `sys/class/drm/card<N>` (connectors such as `card0-DP-1` are
/// not cards), its driver from the `device/driver` link's file name: with `hint`, the first card of
/// that driver only; else the first of `GpuDriver::PREFERENCE` found among the cards whose
/// `device/boot_vga` reads 1 — the one driving the panel, so a hybrid laptop's discrete GPU is never
/// read unless named (docs/decisions.md, "A GPU's usage is read without keeping it awake"). Reads
/// links and PCI attributes only: no file of the GPU itself is opened, so a suspended card stays
/// asleep.
pub fn pick_card(root: &Path, hint: &str) -> Option<Card> {
    let cards: Vec<Card> = entries(&root.join("sys/class/drm"))
        .into_iter()
        .filter(|d| {
            d.file_name()
                .and_then(|n| n.to_str()?.strip_prefix("card"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|dir| {
            let link = std::fs::read_link(dir.join("device/driver")).ok()?;
            let driver = GpuDriver::from_name(link.file_name()?.to_str()?)?;
            Some(Card { dir, driver })
        })
        .collect();
    let (wanted, boot_only) = match hint {
        "" => (GpuDriver::PREFERENCE.to_vec(), true),
        name => (vec![GpuDriver::from_name(name)?], false),
    };
    let boot = |c: &Card| !boot_only || boot_vga(c);
    wanted.into_iter().find_map(|driver| cards.iter().find(|c| c.driver == driver && boot(c)).cloned())
}

/// Whether the card is awake for runtime power management: `active`, `unsupported`, or no
/// `runtime_status` at all. Any other state (`suspended`, `suspending`, `resuming`) means its usage
/// files are not opened: reading them would wake it or hold it up.
pub fn runtime_active(card: &Card) -> bool {
    first_line(&card.dir.join("device/power/runtime_status")).is_none_or(|s| s == "active" || s == "unsupported")
}

/// An amdgpu card's busy percent and VRAM in use (percent, rounded up); each None when its file
/// can't be read.
pub fn read_amdgpu(card: &Card) -> (Option<u8>, Option<u8>) {
    let device = card.dir.join("device");
    let number = |file: &str| first_line(&device.join(file))?.parse::<u64>().ok();
    let busy = number("gpu_busy_percent").map(|p| p.min(100) as u8);
    let vram = match (number("mem_info_vram_used"), number("mem_info_vram_total")) {
        (Some(used), Some(total)) if total > 0 => Some((u128::from(used) * 100).div_ceil(u128::from(total)).min(100) as u8),
        _ => None,
    };
    (busy, vram)
}

/// An Intel card's idle residency, ms (a counter since boot): i915's RC6 residency, from
/// `gt/gt0/` (kernels with per-GT files), else from `power/` (older ones); xe's GT idle residency
/// under its PCI device's `tile0/gt0/gtidle/`. None for amdgpu, or when the file can't be read.
pub fn idle_ms(card: &Card) -> Option<u64> {
    let number = |file: &str| first_line(&card.dir.join(file))?.parse::<u64>().ok();
    match card.driver {
        GpuDriver::I915 => number("gt/gt0/rc6_residency_ms").or_else(|| number("power/rc6_residency_ms")),
        GpuDriver::Xe => number("device/tile0/gt0/gtidle/idle_residency_ms"),
        GpuDriver::Amdgpu => None,
    }
}

/// The share of the time between two idle readings the GPU was not idle, percent: 100 minus the
/// idle share, rounded, clamped to 0..=100. None when no time elapsed or the counter went back (a
/// wrap, or a reset). An estimate (docs/decisions.md, "Intel GPU usage is an idle-residency
/// estimate, labelled active"): a GPU out of its idle state is not necessarily loaded.
pub fn busy_from_idle(prev: (u64, Instant), now: (u64, Instant)) -> Option<u8> {
    let idle = u128::from(now.0.checked_sub(prev.0)?);
    let elapsed = now.1.checked_duration_since(prev.1)?.as_millis();
    if elapsed == 0 {
        return None;
    }
    let idle_share = (idle * 100 + elapsed / 2) / elapsed;
    Some(100u128.saturating_sub(idle_share) as u8)
}

/// Whether the card drives the panel (`device/boot_vga` reads 1).
fn boot_vga(card: &Card) -> bool {
    first_line(&card.dir.join("device/boot_vga")).is_some_and(|b| b == "1")
}

/// The shortest time between two reads of a card's usage or hwmon: its autosuspend delay and a
/// second, so that the card can still fall asleep between them (each read marks it busy). None
/// (every pass) without autosuspend: no readable file, or a negative delay (runtime PM off).
fn spacing(card: &Card) -> Option<Duration> {
    let ms: i64 = first_line(&card.dir.join("device/power/autosuspend_delay_ms"))?.parse().ok()?;
    u64::try_from(ms).ok().map(|ms| Duration::from_millis(ms) + Duration::from_secs(1))
}

/// What a pass of `GpuUsage::read` did, which the hwmon read of `both` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsagePass {
    /// No card: nothing to keep asleep.
    NoCard,
    /// The card is not awake: nothing read, nothing shown.
    Asleep,
    /// Too soon after the last read: the last figures repeat.
    Repeat,
    /// The usage files were read.
    Read,
}

/// The card read for usage (or, in `temp`, only kept for the cadence of a named secondary card's
/// hwmon), picked again (throttled) once its directory is gone; read at most once per `spacing`,
/// never while it is not awake (docs/decisions.md, "A GPU's usage is read without keeping it awake").
struct GpuUsage {
    hint: String,
    card: Option<Card>,
    /// False in `temp`: the gate only, no usage file opened.
    reads_usage: bool,
    spacing: Option<Duration>,
    /// When the usage files were last read; None until then and after the card slept.
    read_at: Option<Instant>,
    /// The figures of the last read, repeated between throttled reads.
    last: (Option<GpuBusy>, Option<u8>),
    /// The hwmon temperature of the last read (`both`), repeated likewise.
    temperature: Option<i16>,
    /// The previous idle reading of an Intel card and when, the estimate's baseline; dropped when the
    /// card sleeps, so no difference spans a suspension.
    idle: Option<(u64, Instant)>,
    /// Ticks left before picking again while there is no card.
    retry_in: u32,
}

impl GpuUsage {
    fn new(hint: &str, card: Option<Card>, reads_usage: bool) -> GpuUsage {
        let mut usage = GpuUsage {
            hint: hint.to_string(),
            card: None,
            reads_usage,
            spacing: None,
            read_at: None,
            last: (None, None),
            temperature: None,
            idle: None,
            retry_in: 0,
        };
        usage.set(card);
        usage
    }

    fn set(&mut self, card: Option<Card>) {
        self.spacing = card.as_ref().and_then(spacing);
        self.retry_in = if card.is_some() { 0 } else { RETRY_EMPTY_EVERY_TICKS };
        self.card = card;
        self.forget();
    }

    fn forget(&mut self) {
        self.read_at = None;
        self.last = (None, None);
        self.temperature = None;
        self.idle = None;
    }

    /// One pass; the figures are in `last`. A card that is gone is picked again at once, no card is
    /// picked again every `RETRY_EMPTY_EVERY_TICKS`.
    fn read(&mut self, root: &Path, now: Instant) -> UsagePass {
        if self.card.as_ref().is_some_and(|c| !c.dir.exists()) {
            self.set(pick_card(root, &self.hint));
        } else if self.card.is_none() {
            match self.retry_in.checked_sub(1) {
                Some(n) => self.retry_in = n,
                None => self.set(pick_card(root, &self.hint)),
            }
        }
        let Some(card) = &self.card else { return UsagePass::NoCard };
        // runtime_status first, before any usage file is opened: opening one wakes a sleeping card
        // (i915, xe) or holds up one going to sleep.
        if !runtime_active(card) {
            self.forget();
            return UsagePass::Asleep;
        }
        let due = match (self.spacing, self.read_at) {
            (Some(spacing), Some(at)) => now.duration_since(at) >= spacing,
            _ => true,
        };
        if !due {
            return UsagePass::Repeat;
        }
        self.read_at = Some(now);
        self.last = match card.driver {
            _ if !self.reads_usage => (None, None),
            GpuDriver::Amdgpu => {
                let (busy, vram) = read_amdgpu(card);
                (busy.map(|percent| GpuBusy { percent, idle_based: false }), vram)
            }
            GpuDriver::I915 | GpuDriver::Xe => {
                let idle = idle_ms(card).map(|ms| (ms, now));
                let busy = self.idle.zip(idle).and_then(|(prev, now)| busy_from_idle(prev, now));
                self.idle = idle;
                (busy.map(|percent| GpuBusy { percent, idle_based: true }), None)
            }
        };
        UsagePass::Read
    }
}

/// Which temperature a `Sensor` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SensorKind {
    Cpu,
    Gpu,
}

/// One temperature, resolved by name and re-resolved when it goes stale (see `Sensor::read`).
struct Sensor {
    kind: SensorKind,
    hint: String,
    /// Each match, alongside the canonicalised `/sys/devices/...` path it
    /// resolved to at the time: hwmon numbers are not stable across a
    /// reboot or a hot-(re)plug, so a bare `hwmonN` can silently start
    /// naming a different device (freed, then reused by something else)
    /// without ever disappearing — `exists()` alone would miss that.
    sensors: Vec<(PathBuf, PathBuf)>,
    /// Ticks left before retrying a pick while `sensors` is empty;
    /// re-resolution driven by a stale path (see `still_resolves`) is not
    /// throttled.
    retry_in: u32,
    /// Whether "nothing matches" is worth a log line this time: once per
    /// such stretch, not once per tick.
    no_match_log: NoMatchLog,
}

pub struct Reader {
    roots: Roots,
    show: Vec<ResourceItem>,
    cpu_sensor: Option<Sensor>,
    gpu_sensor: Option<Sensor>,
    /// The card read for GPU usage, or whose cadence a named secondary card's hwmon keeps in `temp`;
    /// None when `gpu` is not shown or neither applies.
    gpu_usage: Option<GpuUsage>,
    previous: Option<CpuTimes>,
    /// `[resources] net_interface`; None (empty in the file): the default route's interface.
    net_interface: Option<String>,
    /// The interface of the last successful net reading, its counters and when.
    net: Option<(String, NetCounters, Instant)>,
    /// `[resources] disk_mounts`, or `$HOME` alone when the list is empty (nothing without either).
    disk_paths: Vec<PathBuf>,
    /// Which `disk_mounts` paths lie on a network filesystem, decided once at start: never read.
    disk_remote: [bool; DISKS],
    /// The last disk figures, copied into every sample until the next reading.
    disks: [Option<u8>; DISKS],
    /// When the disks were last read; None before the first pass that shows `disk`.
    disks_at: Option<Instant>,
    disk_interval: Duration,
}

/// Does `path` still canonicalise to the device `was` named when resolved?
/// False both when `path` is gone (reboot, hot-unplug) and when it now
/// points elsewhere (hwmon renumbering reusing that number for a different
/// device): either way, the cached path can no longer be trusted.
fn still_resolves(path: &Path, was: &Path) -> bool {
    std::fs::canonicalize(path).is_ok_and(|now| now == was)
}

/// Whether an empty sensor list is worth a log line: the first time, or the
/// first time again since a match was last found. Mirrors `DomainLog`'s
/// first-failure-only rate limiting (sources/dbus/mod.rs).
#[derive(Debug, Default)]
struct NoMatchLog(bool);

impl NoMatchLog {
    /// Records this resolution's result; `true` only the first time `empty`
    /// holds in a row.
    fn note(&mut self, empty: bool) -> bool {
        if !empty {
            self.0 = false;
            return false;
        }
        let first = !self.0;
        self.0 = true;
        first
    }
}

impl Sensor {
    fn new(kind: SensorKind, hint: &str, root: &Path) -> Sensor {
        let mut s = Sensor { kind, hint: hint.to_string(), sensors: Vec::new(), retry_in: 0, no_match_log: NoMatchLog::default() };
        s.resolve(root);
        s
    }

    /// Re-runs the pick and canonicalises every match; logs once if the
    /// result is empty, and arms the throttled empty-list retry.
    fn resolve(&mut self, root: &Path) {
        let picked = match self.kind {
            SensorKind::Cpu => pick_sensor(root, &self.hint),
            SensorKind::Gpu => pick_gpu(root, &self.hint),
        };
        self.sensors = picked.into_iter().filter_map(|p| std::fs::canonicalize(&p).ok().map(|c| (p, c))).collect();
        if self.sensors.is_empty() {
            self.retry_in = RETRY_EMPTY_EVERY_TICKS;
        }
        if self.no_match_log.note(self.sensors.is_empty()) {
            let what = match self.kind {
                SensorKind::Cpu => "temperature sensor",
                SensorKind::Gpu => "GPU sensor",
            };
            log(&format!("resources: no {what} matches {:?}", self.hint));
        }
    }

    /// The max reading across every match; re-resolves by name first when a
    /// cached path no longer resolves to the same device (hwmon is not
    /// stable across a reboot or a hot-(re)plug — never addressed as a bare
    /// `hwmonN`), or, throttled, when nothing has matched so far.
    fn read(&mut self, root: &Path) -> Option<i16> {
        if self.sensors.iter().any(|(p, was)| !still_resolves(p, was)) {
            self.resolve(root);
        } else if self.sensors.is_empty() {
            match self.retry_in.checked_sub(1) {
                Some(n) => self.retry_in = n,
                None => self.resolve(root),
            }
        }
        self.sensors.iter().filter_map(|(p, _)| read_temperature(p)).max()
    }
}

impl Reader {
    /// Resolves the sensors once, by name, at startup.
    pub fn new(roots: Roots, config: &ResourcesConfig) -> Reader {
        let show = config.show.clone();
        let sensor = |item, kind, hint: &str| show.contains(&item).then(|| Sensor::new(kind, hint, &roots.root));
        let cpu_sensor = sensor(ResourceItem::Temperature, SensorKind::Cpu, &config.sensor);
        // What `gpu` reads is decided once, here: `auto` reads usage when a card offering it is
        // there at start, else the temperature, never both.
        let hint = &config.gpu_sensor;
        let (gpu_sensor, gpu_usage) = match (show.contains(&ResourceItem::Gpu), config.gpu_metric) {
            (false, _) => (None, None),
            // A named secondary card's hwmon keeps the usage read's cadence: it marks the card busy too.
            (true, GpuMetric::Temp) => (
                sensor(ResourceItem::Gpu, SensorKind::Gpu, hint),
                pick_card(&roots.root, hint).filter(|c| !hint.is_empty() && !boot_vga(c)).map(|c| GpuUsage::new(hint, Some(c), false)),
            ),
            (true, GpuMetric::Usage) => (None, Some(GpuUsage::new(hint, pick_card(&roots.root, hint), true))),
            (true, GpuMetric::Both) => {
                (sensor(ResourceItem::Gpu, SensorKind::Gpu, hint), Some(GpuUsage::new(hint, pick_card(&roots.root, hint), true)))
            }
            (true, GpuMetric::Auto) => match pick_card(&roots.root, hint) {
                Some(card) => (None, Some(GpuUsage::new(hint, Some(card), true))),
                None => (sensor(ResourceItem::Gpu, SensorKind::Gpu, hint), None),
            },
        };
        let net_interface = Some(config.net_interface.clone()).filter(|n| !n.is_empty());
        // Kept only when `disk` is shown: an item left out keeps no state.
        let disk_paths = match show.contains(&ResourceItem::Disk) {
            false => Vec::new(),
            true if config.disk_mounts.is_empty() => roots.home.iter().cloned().collect(),
            true => config.disk_mounts.iter().take(DISKS).cloned().collect(),
        };
        let mut disk_remote = [false; DISKS];
        if show.contains(&ResourceItem::Disk) && !config.disk_mounts.is_empty() {
            let mountinfo = std::fs::read_to_string(roots.root.join("proc/self/mountinfo")).unwrap_or_default();
            for (remote, path) in disk_remote.iter_mut().zip(&disk_paths) {
                *remote = on_network_fs(&mountinfo, path);
            }
        }
        Reader {
            roots,
            show,
            cpu_sensor,
            gpu_sensor,
            gpu_usage,
            previous: None,
            net_interface,
            net: None,
            disk_paths,
            disk_remote,
            disks: [None; DISKS],
            disks_at: None,
            disk_interval: Duration::from_secs(config.disk_interval),
        }
    }

    /// Whether the disks are read on this pass: the first one, then once `disk_interval` has passed
    /// since the last reading.
    fn disk_due(&self, now: Instant) -> bool {
        self.disks_at.is_none_or(|at| now.saturating_duration_since(at) >= self.disk_interval)
    }

    fn shows(&self, item: ResourceItem) -> bool {
        self.show.contains(&item)
    }

    /// The first temperature match, diagnostics only: `sample` reads every match and keeps the max.
    pub fn sensor(&self) -> Option<&Path> {
        self.cpu_sensor.as_ref()?.sensors.first().map(|(p, _)| p.as_path())
    }

    /// Net throughput of the named interface, or else of the default route's, against the last
    /// reading of that same interface; the new counters replace the old ones on every successful
    /// read. A named interface never reads /proc/net/route.
    fn net(&mut self, now: Instant) -> Option<NetRate> {
        // A named interface is borrowed: no allocation per tick; only a change of interface (or the
        // first reading) stores an owned name.
        let routed;
        let iface: &str = match &self.net_interface {
            Some(name) => name,
            None => {
                routed = std::fs::read_to_string(self.roots.root.join("proc/net/route")).ok().and_then(|t| default_route(&t));
                match &routed {
                    Some(name) => name,
                    None => {
                        self.net = None;
                        return None;
                    }
                }
            }
        };
        let counters = std::fs::read_to_string(self.roots.root.join("proc/net/dev")).ok().and_then(|t| net_counters(&t, iface));
        let Some(counters) = counters else {
            self.net = None;
            return None;
        };
        match &mut self.net {
            Some((was, prev, at)) if was == iface => {
                let rate = net_rate(*prev, counters, now.saturating_duration_since(*at));
                (*prev, *at) = (counters, now);
                rate
            }
            slot => {
                *slot = Some((iface.to_owned(), counters, now));
                None
            }
        }
    }

    /// One reading at the current time.
    pub fn sample(&mut self) -> Sample {
        self.sample_at(Instant::now())
    }

    /// One reading; each figure only when its item is in `show` (the others stay None, nothing read).
    pub(crate) fn sample_at(&mut self, now: Instant) -> Sample {
        let mut sample = Sample::default();
        if self.shows(ResourceItem::Cpu) {
            let now = first_line(&self.roots.root.join("proc/stat")).and_then(|l| parse_stat(&l));
            sample.cpu = match (self.previous, now) {
                (Some(a), Some(b)) => cpu_percent(a, b),
                _ => None,
            };
            self.previous = now;
        }
        if self.shows(ResourceItem::Memory)
            && let Ok(text) = std::fs::read_to_string(self.roots.root.join("proc/meminfo"))
        {
            sample.memory = parse_meminfo(&text);
            sample.memory_total = parse_mem_total(&text);
        }
        if self.shows(ResourceItem::Temperature) {
            sample.temperature = self.cpu_sensor.as_mut().and_then(|s| s.read(&self.roots.root));
        }
        if self.shows(ResourceItem::Disk) {
            if self.disk_due(now) {
                // A failing call leaves that figure out until the next reading. `statvfs` runs on
                // the loop, so a listed path on a network filesystem is never asked (a hung server
                // would stall it); `$HOME` is, whatever it lies on.
                for ((slot, path), remote) in self.disks.iter_mut().zip(&self.disk_paths).zip(self.disk_remote) {
                    *slot = if remote { None } else { (self.roots.statfs)(path).and_then(disk_percent) };
                }
                self.disks_at = Some(now);
            }
            sample.disks = self.disks;
        }
        if self.shows(ResourceItem::Net) {
            sample.net = self.net(now);
        }
        if self.shows(ResourceItem::Gpu) {
            let root = &self.roots.root;
            let mut sensor = || self.gpu_sensor.as_mut().and_then(|s| s.read(root));
            match self.gpu_usage.as_mut() {
                None => sample.gpu = sensor(),
                Some(usage) => {
                    // The hwmon read (`both`, or `temp` with a named secondary card) follows the
                    // usage read: a card's sensor marks it busy as its usage file does.
                    sample.gpu = match usage.read(root, now) {
                        UsagePass::NoCard => sensor(),
                        UsagePass::Read => {
                            usage.temperature = sensor();
                            usage.temperature
                        }
                        UsagePass::Repeat => usage.temperature,
                        UsagePass::Asleep => None,
                    };
                    (sample.gpu_busy, sample.vram) = usage.last;
                }
            }
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const STAT_A: &str = include_str!("../../tests/fixtures/proc-stat-a.txt");
    const STAT_B: &str = include_str!("../../tests/fixtures/proc-stat-b.txt");
    const MEMINFO: &str = include_str!("../../tests/fixtures/proc-meminfo.txt");

    fn refuse(_: &Path) -> Option<FsBlocks> {
        panic!("statvfs read for an item not in show")
    }

    fn roots(root: &Path, home: Option<&str>, statfs: StatFs) -> Roots {
        Roots { root: root.to_path_buf(), home: home.map(PathBuf::from), statfs }
    }

    fn default_reader(root: &Path, hint: &str) -> Reader {
        let config = ResourcesConfig { sensor: hint.to_string(), ..ResourcesConfig::default() };
        Reader::new(roots(root, None, refuse), &config)
    }

    fn disk_reader(root: &Path, home: Option<&str>, statfs: StatFs) -> Reader {
        let config = ResourcesConfig { show: vec![ResourceItem::Disk], ..ResourcesConfig::default() };
        Reader::new(roots(root, home, statfs), &config)
    }

    fn fake(p: &Path) -> Option<FsBlocks> {
        (p == Path::new("/fictional/home")).then_some(FsBlocks { blocks: 1000, free: 380, avail: 380 })
    }

    fn put(root: &Path, file: &str, text: &str) {
        let p = root.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    struct Tree(PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The workstation's sensors as listed on 2026-10-04 (ls
    /// /sys/class/{hwmon,thermal}, name and type files), trimmed.
    fn workstation(name: &str) -> Tree {
        let r = std::env::temp_dir().join(format!("alpheus-resources-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        for (f, t) in [
            ("sys/class/hwmon/hwmon0/name", "nvme\n"),
            ("sys/class/hwmon/hwmon0/temp1_input", "40850\n"),
            ("sys/class/hwmon/hwmon4/name", "acpitz\n"),
            ("sys/class/hwmon/hwmon4/temp1_input", "67000\n"),
            ("sys/class/hwmon/hwmon9/name", "coretemp\n"),
            ("sys/class/hwmon/hwmon9/temp1_label", "Package id 0\n"),
            ("sys/class/hwmon/hwmon9/temp1_input", "66000\n"),
            ("sys/class/hwmon/hwmon9/temp10_input", "64000\n"),
            // The Wi-Fi card has both an hwmon and a thermal zone named iwlwifi_1.
            ("sys/class/hwmon/hwmon13/name", "iwlwifi_1\n"),
            ("sys/class/hwmon/hwmon13/temp1_input", "72000\n"),
            ("sys/class/hwmon/hwmon16/name", "hidpp_battery_1\n"),
            ("sys/class/thermal/thermal_zone0/type", "INT3400 Thermal\n"),
            ("sys/class/thermal/thermal_zone0/temp", "20000\n"),
            ("sys/class/thermal/thermal_zone6/type", "x86_pkg_temp\n"),
            ("sys/class/thermal/thermal_zone6/temp", "68000\n"),
            ("sys/class/thermal/thermal_zone9/type", "iwlwifi_1\n"),
            ("sys/class/thermal/thermal_zone9/temp", "72000\n"),
            ("sys/class/thermal/cooling_device0/type", "Processor\n"),
            ("proc/stat", STAT_A),
            ("proc/meminfo", MEMINFO),
        ] {
            put(&r, f, t);
        }
        Tree(r)
    }

    #[test]
    fn cpu_between_two_readings_of_the_workstation() {
        let a = parse_stat(STAT_A.lines().next().unwrap()).unwrap();
        let b = parse_stat(STAT_B.lines().next().unwrap()).unwrap();
        assert_eq!(b.total - a.total, 4651);
        assert_eq!(cpu_percent(a, b), Some(38));
        assert_eq!(cpu_percent(b, a), None, "counters going back");
        assert_eq!(cpu_percent(a, a), None, "no time elapsed");
        assert_eq!(parse_stat("cpu0 1 2 3 4 5"), None, "not the aggregate line");
        assert_eq!(parse_stat("cpu  1 x 3 4 5"), None);
    }

    #[test]
    fn mem_total_in_tenths_of_gib() {
        let kb: u64 = MEMINFO.lines().find_map(|l| l.strip_prefix("MemTotal:")?.trim().strip_suffix("kB")?.trim().parse().ok()).unwrap();
        let expected = ((kb as f64 / (1024.0 * 1024.0)) * 10.0).round() as u16;
        assert_eq!(parse_mem_total(MEMINFO), Some(expected));
        assert_eq!(parse_mem_total("MemTotal: 1781760 kB\n"), Some(17), "1.70 GiB");
        assert_eq!(parse_mem_total("MemTotal: 1730151 kB\n"), Some(17), "1.65 GiB rounds up");
        assert_eq!(parse_mem_total("MemTotal: 0 kB\n"), None);
        assert_eq!(parse_mem_total(""), None);
    }

    #[test]
    fn memory_in_use_like_htop_without_cache() {
        // 2026-10-06: RAM only, like htop: MemTotal − MemFree − Buffers
        // − (Cached + SReclaimable − Shmem) = 10574668 kB on the workstation
        // fixture, 10.1 GiB (MemTotal − MemAvailable would say 10.5).
        assert_eq!(parse_meminfo(MEMINFO), Some(101), "10.1 GiB in use, caches left out, shared memory kept");
        assert_eq!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn a_big_tmpfs_stays_counted_as_used() {
        // 4 GiB of tmpfs sits in Cached but cannot be reclaimed: still in use.
        let gib = 1024 * 1024;
        let text = format!(
            "MemTotal: {} kB\nMemFree: {} kB\nBuffers: 0 kB\nCached: {} kB\nSReclaimable: 0 kB\nShmem: {} kB\n",
            16 * gib,
            8 * gib,
            6 * gib,
            4 * gib
        );
        assert_eq!(parse_meminfo(&text), Some(60), "2 GiB of programs + 4 GiB of tmpfs");
    }

    #[test]
    fn memory_falls_back_to_memavailable_without_the_cache_fields() {
        let partial = "MemTotal: 10485760 kB\nMemFree: 1048576 kB\nMemAvailable: 5242880 kB\n";
        assert_eq!(parse_meminfo(partial), Some(50), "no Buffers/Cached/SReclaimable: MemTotal − MemAvailable");
    }

    #[test]
    fn memory_falls_back_to_memfree_without_memavailable() {
        // Pre-3.14 kernels have no MemAvailable at all; the same fixture with that line (and the
        // cache lines) dropped, so the figure below is MemTotal − MemFree on it.
        let no_available: String = MEMINFO
            .lines()
            .filter(|l| !["MemAvailable:", "Buffers:", "Cached:", "SReclaimable:"].iter().any(|k| l.starts_with(k)))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_meminfo(&no_available), Some(217), "MemFree instead, old kernels");
        assert_eq!(parse_meminfo("MemTotal: 1000 kB\n"), None, "neither MemAvailable nor MemFree");
    }

    #[test]
    fn the_sensor_automatic_or_by_name() {
        let t = workstation("sensors");
        assert_eq!(pick_sensor(&t.0, ""), vec![t.0.join("sys/class/hwmon/hwmon9/temp1_input")], "coretemp's package");
        assert_eq!(pick_sensor(&t.0, "x86_pkg_temp"), vec![t.0.join("sys/class/thermal/thermal_zone6/temp")], "a zone type");
        assert_eq!(pick_sensor(&t.0, "iwlwifi_1"), vec![t.0.join("sys/class/hwmon/hwmon13/temp1_input")], "hwmon before the zones");
        assert_eq!(pick_sensor(&t.0, "acpitz"), vec![t.0.join("sys/class/hwmon/hwmon4/temp1_input")]);
        assert_eq!(pick_sensor(&t.0, "weather"), Vec::<PathBuf>::new());
        std::fs::remove_dir_all(t.0.join("sys/class/hwmon/hwmon9")).unwrap();
        assert_eq!(pick_sensor(&t.0, ""), vec![t.0.join("sys/class/thermal/thermal_zone6/temp")], "no coretemp: the package zone");
        assert_eq!(pick_sensor(Path::new("/nonexistent"), ""), Vec::<PathBuf>::new());
    }

    #[test]
    fn k10temp_prefers_tdie_present_then_tctl() {
        // AMD: temp1_input is Tctl (offset, hotter); prefer the labelled Tdie
        // when the driver exposes it, else fall back to Tctl.
        let tdie = |has_tdie: bool, name: &str| -> Tree {
            let r = std::env::temp_dir().join(format!("alpheus-resources-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&r);
            put(&r, "sys/class/hwmon/hwmon2/name", "k10temp\n");
            put(&r, "sys/class/hwmon/hwmon2/temp1_label", "Tctl\n");
            put(&r, "sys/class/hwmon/hwmon2/temp1_input", "71000\n");
            if has_tdie {
                put(&r, "sys/class/hwmon/hwmon2/temp2_label", "Tdie\n");
                put(&r, "sys/class/hwmon/hwmon2/temp2_input", "65000\n");
            }
            Tree(r)
        };
        let t = tdie(true, "tdie");
        assert_eq!(pick_sensor(&t.0, "k10temp"), vec![t.0.join("sys/class/hwmon/hwmon2/temp2_input")], "Tdie over Tctl");
        let t = tdie(false, "tctl");
        assert_eq!(pick_sensor(&t.0, "k10temp"), vec![t.0.join("sys/class/hwmon/hwmon2/temp1_input")], "no Tdie: Tctl");
    }

    #[test]
    fn several_matching_packages_take_the_max() {
        let r = std::env::temp_dir().join(format!("alpheus-resources-{}-packages", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        let t = Tree(r);
        put(&t.0, "sys/class/hwmon/hwmon9/name", "coretemp\n");
        put(&t.0, "sys/class/hwmon/hwmon9/temp1_input", "60000\n"); // package 0
        put(&t.0, "sys/class/hwmon/hwmon10/name", "coretemp\n");
        put(&t.0, "sys/class/hwmon/hwmon10/temp1_input", "71000\n"); // package 1, hotter
        put(&t.0, "proc/stat", STAT_A);
        put(&t.0, "proc/meminfo", MEMINFO);
        assert_eq!(
            pick_sensor(&t.0, ""),
            vec![t.0.join("sys/class/hwmon/hwmon10/temp1_input"), t.0.join("sys/class/hwmon/hwmon9/temp1_input")],
            "both packages matched"
        );
        let mut reader = default_reader(&t.0, "");
        assert_eq!(reader.sample().temperature, Some(71), "the hotter package");
    }

    #[test]
    fn the_reader_first_without_cpu_then_38() {
        let t = workstation("reader");
        let mut r = default_reader(&t.0, "");
        assert_eq!(
            r.sample(),
            Sample { cpu: None, memory: Some(101), memory_total: Some(467), temperature: Some(66), ..Sample::default() }
        );
        put(&t.0, "proc/stat", STAT_B);
        assert_eq!(
            r.sample(),
            Sample { cpu: Some(38), memory: Some(101), memory_total: Some(467), temperature: Some(66), ..Sample::default() }
        );
        put(&t.0, "sys/class/hwmon/hwmon9/temp1_input", "-5000\n");
        assert_eq!(r.sample().temperature, Some(-5));
    }

    #[test]
    fn hwmon_renumbering_is_not_cached_across_a_reload() {
        let t = workstation("renumber");
        let mut r = default_reader(&t.0, "");
        assert_eq!(r.sample().temperature, Some(66), "hwmon9, coretemp");
        // Never a boot-stable number: the same sensor comes back under a
        // different hwmonN (a reboot, or a dock's hwmon shifting the index).
        std::fs::rename(t.0.join("sys/class/hwmon/hwmon9"), t.0.join("sys/class/hwmon/hwmon2")).unwrap();
        assert_eq!(r.sample().temperature, Some(66), "re-resolved by name, not the stale hwmon9 path");
    }

    #[test]
    fn nothing_readable_gives_an_empty_sample() {
        let mut r = default_reader(Path::new("/nonexistent"), "");
        assert_eq!(r.sensor(), None);
        assert_eq!(r.sample(), Sample::default());
    }

    /// `/sys/class/hwmon/hwmonN` is really a symlink into
    /// `/sys/devices/.../hwmonN`; the kernel can repoint that symlink at a
    /// different device when it reuses the number, without the class path
    /// ever disappearing in between — `.exists()` alone would see nothing
    /// wrong and keep reading the new (unrelated) device's file under the
    /// old name. Canonicalising and comparing against the device path
    /// recorded at resolution time catches it.
    #[test]
    fn hwmon_number_reused_by_a_different_device_without_ever_disappearing() {
        let r = std::env::temp_dir().join(format!("alpheus-resources-{}-reused", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        put(&r, "sys/devices/deviceA/hwmon9/name", "coretemp\n");
        put(&r, "sys/devices/deviceA/hwmon9/temp1_input", "66000\n");
        // Device B is not one `pick_sensor` would ever choose by name: once
        // hwmon9 names it instead, re-resolving finds nothing coretemp-like.
        put(&r, "sys/devices/deviceB/hwmon9/name", "nvme\n");
        put(&r, "sys/devices/deviceB/hwmon9/temp1_input", "40850\n");
        std::fs::create_dir_all(r.join("sys/class/hwmon")).unwrap();
        std::os::unix::fs::symlink(r.join("sys/devices/deviceA/hwmon9"), r.join("sys/class/hwmon/hwmon9")).unwrap();
        put(&r, "proc/stat", STAT_A);
        put(&r, "proc/meminfo", MEMINFO);
        let t = Tree(r);
        let mut reader = default_reader(&t.0, "");
        assert_eq!(reader.sample().temperature, Some(66), "device A, coretemp");
        // hwmon9 repointed to device B: the class symlink itself is never
        // absent at any point an outside observer could check `.exists()`.
        std::fs::remove_file(t.0.join("sys/class/hwmon/hwmon9")).unwrap();
        std::os::unix::fs::symlink(t.0.join("sys/devices/deviceB/hwmon9"), t.0.join("sys/class/hwmon/hwmon9")).unwrap();
        assert_eq!(reader.sample().temperature, None, "re-resolved: hwmon9 now names an nvme, not coretemp");
    }

    #[test]
    fn empty_sensor_list_is_retried_but_throttled() {
        let r = std::env::temp_dir().join(format!("alpheus-resources-{}-retry", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        put(&r, "proc/stat", STAT_A);
        put(&r, "proc/meminfo", MEMINFO);
        let t = Tree(r);
        let mut reader = default_reader(&t.0, "coretemp"); // nothing matches yet
        assert_eq!(reader.sample().temperature, None);
        // The sensor appears later (driver module loaded, device
        // hot-plugged): picked up eventually, but not re-scanned on every
        // following tick — that would mean walking hwmon and thermal every
        // tick forever for a sensor that may never exist.
        put(&t.0, "sys/class/hwmon/hwmon9/name", "coretemp\n");
        put(&t.0, "sys/class/hwmon/hwmon9/temp1_input", "66000\n");
        let found_at = (0..RETRY_EMPTY_EVERY_TICKS).find(|_| reader.sample().temperature.is_some());
        assert!(found_at.is_some(), "never retried within {RETRY_EMPTY_EVERY_TICKS} ticks of the sensor appearing");
        assert_ne!(found_at, Some(0), "retried on the very next tick: not throttled at all");
    }

    #[test]
    fn no_match_log_fires_once_per_stretch() {
        let mut log = NoMatchLog::default();
        assert!(log.note(true), "the first empty resolution is worth a line");
        assert!(!log.note(true), "still empty: not worth a line again");
        assert!(!log.note(true), "and again: still quiet");
        assert!(!log.note(false), "a match: nothing to log, and this resets the state");
        assert!(log.note(true), "empty again after a match: worth a line again");
    }

    #[test]
    fn disk_percent_rounds_up_like_df() {
        let fs = |blocks, free, avail| FsBlocks { blocks, free, avail };
        assert_eq!(disk_percent(fs(1000, 380, 380)), Some(62));
        assert_eq!(disk_percent(fs(1000, 400, 350)), Some(64));
        assert_eq!(disk_percent(fs(1000, 1000, 1000)), Some(0));
        assert_eq!(disk_percent(fs(1000, 0, 0)), Some(100));
        assert_eq!(disk_percent(fs(0, 0, 0)), None);
        assert_eq!(disk_percent(fs(u64::MAX, 0, 0)), Some(100), "no overflow");
    }

    #[test]
    fn no_mounts_still_measures_home() {
        let t = workstation("disk");
        assert_eq!(disk_reader(&t.0, Some("/fictional/home"), fake).sample().disks, [Some(62), None, None, None]);
        assert_eq!(disk_reader(&t.0, None, fake).sample().disks, [None; DISKS], "no $HOME, no figure");
    }

    fn mounts_reader(root: &Path, mounts: &[&str], statfs: StatFs) -> Reader {
        let config = ResourcesConfig {
            show: vec![ResourceItem::Disk],
            disk_mounts: mounts.iter().map(PathBuf::from).collect(),
            ..ResourcesConfig::default()
        };
        Reader::new(roots(root, Some("/fictional/home"), statfs), &config)
    }

    #[test]
    fn disk_mounts_statfs_only_the_listed_paths() {
        fn listed(p: &Path) -> Option<FsBlocks> {
            match p.to_str() {
                Some("/fictional/a") => Some(FsBlocks { blocks: 1000, free: 380, avail: 380 }),
                Some("/fictional/b") => Some(FsBlocks { blocks: 1000, free: 900, avail: 900 }),
                _ => panic!("statvfs of a path not listed"),
            }
        }
        let t = workstation("mounts");
        let mut r = mounts_reader(&t.0, &["/fictional/a", "/fictional/b"], listed);
        assert_eq!(r.sample().disks, [Some(62), Some(10), None, None]);
    }

    /// A trimmed /proc/self/mountinfo: local ext4 and btrfs, an NFS share with a btrfs mounted inside
    /// it, an sshfs, a CIFS share whose mount point holds a space, a tmpfs mounted over the NFS
    /// share's `over` directory, an automount point not yet mounted, the other network types, an
    /// rclone and a gocryptfs FUSE mount, and a `fuseblk` USB disk. Fictional paths and devices.
    const MOUNTINFO: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/fictional2 rw
30 22 259:3 / /fictional/data rw,relatime shared:2 - btrfs /dev/fictional3 rw
41 22 0:50 / /fictional/nfs rw,relatime shared:3 - nfs4 server:/export rw,vers=4.2
42 41 259:4 / /fictional/nfs/local rw,relatime shared:4 - btrfs /dev/fictional4 rw
43 22 0:51 / /fictional/ssh rw,nosuid shared:5 - fuse.sshfs host:/ rw
44 22 0:52 / /fictional/my\\040share rw shared:6 - cifs //server/share rw
45 41 0:53 / /fictional/nfs/over rw shared:7 - nfs4 server:/other rw
46 41 0:54 / /fictional/nfs/over rw shared:8 - tmpfs tmpfs rw
50 22 0:60 / /fictional/nas rw,relatime shared:9 - autofs systemd-1 rw,fd=50,pgrp=1,timeout=0
51 22 0:61 / /fictional/smb rw shared:10 - smbfs //server/old rw
52 22 0:62 / /fictional/dav rw shared:11 - davfs https://server/dav rw
53 22 0:63 / /fictional/afs rw shared:12 - afs AFS rw
54 22 0:64 / /fictional/lustre rw shared:13 - lustre server@tcp:/fs rw
55 22 0:65 / /fictional/rclone rw shared:14 - fuse.rclone remote: rw
56 22 0:66 / /fictional/vault rw shared:15 - fuse.gocryptfs /fictional/data/cipher rw
57 22 8:17 / /fictional/usb rw shared:16 - fuseblk /dev/fictional5 rw
";

    #[test]
    fn a_network_mount_is_found_by_its_longest_mount_point() {
        let remote = |p: &str| on_network_fs(MOUNTINFO, Path::new(p));
        assert!(!remote("/"));
        assert!(!remote("/fictional/data/photos"));
        assert!(remote("/fictional/nfs"));
        assert!(remote("/fictional/nfs/deep/dir"));
        assert!(!remote("/fictional/nfs/local"), "a local filesystem mounted inside the share");
        assert!(!remote("/fictional/nfs/over"), "the later of two mounts on one point wins");
        assert!(!remote("/fictional/nfsx"), "matched by components, not by text");
        assert!(remote("/fictional/ssh/home"));
        assert!(remote("/fictional/my share"), "the mount point's escapes are undone");
        assert!(remote("/fictional/nas/films"), "an automount point: a statvfs would mount the share");
        for p in ["/fictional/smb", "/fictional/dav", "/fictional/afs", "/fictional/lustre"] {
            assert!(remote(p), "{p}");
        }
        assert!(remote("/fictional/rclone"), "an unknown FUSE type counts as remote");
        assert!(!remote("/fictional/vault"), "a FUSE type known to be local");
        assert!(!remote("/fictional/usb"), "fuseblk sits on a block device");
        assert!(!on_network_fs("", Path::new("/fictional/nfs")), "no mountinfo: nothing is skipped");
    }

    #[test]
    fn a_disk_mount_on_a_network_filesystem_is_never_statvfs_ed() {
        fn local_only(p: &Path) -> Option<FsBlocks> {
            match p.to_str() {
                Some("/fictional/data") => Some(FsBlocks { blocks: 1000, free: 380, avail: 380 }),
                _ => panic!("statvfs of a network mount"),
            }
        }
        let t = workstation("netfs");
        put(&t.0, "proc/self/mountinfo", MOUNTINFO);
        let mut r = mounts_reader(&t.0, &["/fictional/nfs/deep", "/fictional/data", "/fictional/ssh"], local_only);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).disks, [None, Some(62), None, None]);
        assert_eq!(r.sample_at(t0 + Duration::from_secs(15)).disks, [None, Some(62), None, None], "not on a later read either");
    }

    #[test]
    fn disks_are_read_once_per_disk_interval() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_: &Path) -> Option<FsBlocks> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Some(FsBlocks { blocks: 1000, free: 380, avail: 380 })
        }
        let t = workstation("cadence");
        let mut r = mounts_reader(&t.0, &["/fictional/a"], counting);
        assert_eq!(r.disk_interval, Duration::from_secs(15), "the default cadence");
        let t0 = Instant::now();
        for s in [0, 2, 14] {
            let sample = r.sample_at(t0 + Duration::from_secs(s));
            assert_eq!(sample.disks[0], Some(62), "the last figure repeats at {s} s");
            assert_eq!(CALLS.load(Ordering::SeqCst), 1, "read once until 15 s, at {s} s");
        }
        assert_eq!(r.sample_at(t0 + Duration::from_secs(15)).disks[0], Some(62));
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "read again once 15 s have passed");
    }

    #[test]
    fn a_failing_statfs_hides_the_disk_and_is_retried() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn flaky(_: &Path) -> Option<FsBlocks> {
            (CALLS.fetch_add(1, Ordering::SeqCst) > 0).then_some(FsBlocks { blocks: 1000, free: 380, avail: 380 })
        }
        let t = workstation("flaky");
        let mut r = disk_reader(&t.0, Some("/fictional/home"), flaky);
        // Due on every pass while the interval is 1 s and each pass is 1 s apart.
        r.disk_interval = Duration::from_secs(1);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).disks[0], None);
        assert_eq!(r.sample_at(t0 + Duration::from_secs(1)).disks[0], Some(62));
    }

    #[test]
    fn nothing_is_read_for_an_item_not_in_show() {
        let t = workstation("noread");
        let mut r = disk_reader(&t.0, Some("/fictional/home"), fake);
        r.sample();
        put(&t.0, "proc/stat", STAT_B); // a second reading would give a CPU figure if cpu were read
        let s = r.sample();
        assert_eq!((s.cpu, s.memory, s.temperature), (None, None, None));
        assert_eq!(r.sensor(), None, "no sensor resolved without Temperature");
        // A $HOME is known, yet the default show has no Disk: `refuse` would panic on a statvfs.
        let mut d = Reader::new(roots(&t.0, Some("/fictional/home"), refuse), &ResourcesConfig::default());
        d.sample();
        assert_eq!(d.sample().disks, [None; DISKS]);
        // Mounts listed, yet no Disk in show: still not one statvfs.
        let config = ResourcesConfig { disk_mounts: vec![PathBuf::from("/fictional/a")], ..ResourcesConfig::default() };
        let mut d = Reader::new(roots(&t.0, Some("/fictional/home"), refuse), &config);
        d.sample();
        assert_eq!(d.sample().disks, [None; DISKS]);
    }

    #[test]
    fn the_reader_reads_the_total_with_memory_only() {
        let t = workstation("memtotal");
        let expected = parse_mem_total(MEMINFO);
        assert!(expected.is_some());
        assert_eq!(default_reader(&t.0, "").sample().memory_total, expected);
        let config = ResourcesConfig { show: vec![ResourceItem::Cpu], ..ResourcesConfig::default() };
        let mut r = Reader::new(roots(&t.0, None, refuse), &config);
        assert_eq!(r.sample().memory_total, None);
    }

    const NET_A: &str = include_str!("../../tests/fixtures/proc-net-dev-a.txt");
    const NET_B: &str = include_str!("../../tests/fixtures/proc-net-dev-b.txt");

    const ROUTE_HEAD: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
    const ROUTE_ETH0: &str = "eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
    const ROUTE_WLAN0: &str = "wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";
    const ROUTE_SUBNET: &str = "wlan0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn default_route_takes_the_lowest_metric() {
        let route = |lines: &[&str]| format!("{ROUTE_HEAD}{}", lines.concat());
        assert_eq!(default_route(&route(&[ROUTE_ETH0, ROUTE_WLAN0, ROUTE_SUBNET])), Some("eth0".to_string()));
        assert_eq!(default_route(&route(&[ROUTE_WLAN0, ROUTE_SUBNET])), Some("wlan0".to_string()));
        let down = ROUTE_ETH0.replace("0003", "0002");
        assert_eq!(default_route(&route(&[&down, ROUTE_WLAN0, ROUTE_SUBNET])), Some("wlan0".to_string()), "a down route is skipped");
        assert_eq!(default_route(&route(&[ROUTE_SUBNET])), None, "no default route");
        assert_eq!(default_route(""), None);
    }

    #[test]
    fn default_route_first_line_wins_a_metric_tie() {
        let tie = format!("{ROUTE_HEAD}{ROUTE_WLAN0}{}", ROUTE_ETH0.replace("100", "600"));
        assert_eq!(default_route(&tie), Some("wlan0".to_string()));
    }

    #[test]
    fn net_counters_glued_name_and_exact_match() {
        let tail = "123 1 0 0 0 0 0 0 456 1 0 0 0 0 0 0";
        // A 15-character name leaves no space before its first counter.
        let dev = format!(
            "Inter-|   Receive\n face |bytes\n verylongifname:{tail}\n  wlan0.1: 7 1 0 0 0 0 0 0 8 1 0 0 0 0 0 0\n  wlan0: 9 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0\n"
        );
        assert_eq!(net_counters(&dev, "verylongifname"), Some(NetCounters { rx: 123, tx: 456 }));
        assert_eq!(net_counters(&dev, "wlan0"), Some(NetCounters { rx: 9, tx: 10 }), "not wlan0.1");
        assert_eq!(net_counters(&dev, "wlan0.1"), Some(NetCounters { rx: 7, tx: 8 }));
        let only_dotted = "  wlan0.1: 7 1 0 0 0 0 0 0 8 1 0 0 0 0 0 0\n";
        assert_eq!(net_counters(only_dotted, "wlan0"), None);
    }

    #[test]
    fn net_counters_of_an_interface() {
        assert_eq!(net_counters(NET_A, "wlan0"), Some(NetCounters { rx: 1_000_000, tx: 200_000 }));
        assert_eq!(net_counters(NET_A, "wlan"), None, "no prefix match");
        assert_eq!(net_counters(NET_A, "usb0"), None);
    }

    #[test]
    fn net_rate_between_two_readings() {
        let a = net_counters(NET_A, "wlan0").unwrap();
        let b = net_counters(NET_B, "wlan0").unwrap();
        assert_eq!(net_rate(a, b, Duration::from_secs(2)), Some(NetRate { down: 1_258_291, up: 81_920 }));
        assert_eq!(net_rate(b, a, Duration::from_secs(2)), None, "counters reset");
        assert_eq!(net_rate(a, b, Duration::ZERO), None);
    }

    fn net_reader(root: &Path) -> Reader {
        let config = ResourcesConfig { show: vec![ResourceItem::Net], ..ResourcesConfig::default() };
        Reader::new(roots(root, None, refuse), &config)
    }

    #[test]
    fn the_reader_shows_net_from_the_second_pass() {
        let t = workstation("net");
        put(&t.0, "proc/net/route", &format!("{ROUTE_HEAD}{ROUTE_WLAN0}"));
        put(&t.0, "proc/net/dev", NET_A);
        let mut r = net_reader(&t.0);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).net, None, "the first pass only records");
        put(&t.0, "proc/net/dev", NET_B);
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).net, Some(NetRate { down: 1_258_291, up: 81_920 }));
        put(&t.0, "proc/net/route", &format!("{ROUTE_HEAD}{ROUTE_ETH0}"));
        // eth0 far ahead of wlan0: comparing across interfaces would give a rate, not None.
        let eth0_ahead = NET_B.replace("  eth0:  900000", "  eth0: 9000000").replace("   150000     800", "  9000000     800");
        assert_ne!(eth0_ahead, NET_B);
        put(&t.0, "proc/net/dev", &eth0_ahead);
        assert_eq!(r.sample_at(t0 + Duration::from_secs(4)).net, None, "another interface: nothing to compare");
        put(&t.0, "proc/net/dev", &eth0_ahead.replace("  eth0: 9000000", "  eth0: 9500000"));
        assert!(r.sample_at(t0 + Duration::from_secs(6)).net.is_some());
        std::fs::remove_file(t.0.join("proc/net/route")).unwrap();
        assert_eq!(r.sample_at(t0 + Duration::from_secs(8)).net, None, "no route");
    }

    /// Two fictional interfaces in /proc/net/dev and a /proc/net/route that cannot be read (a
    /// directory): a figure can only come from the named interface.
    fn named_tree(name: &str) -> Tree {
        let t = workstation(name);
        std::fs::create_dir_all(t.0.join("proc/net/route")).unwrap();
        t
    }

    const DEV_HEAD: &str = "Inter-|   Receive\n face |bytes\n";

    fn named_dev(rx0: u64, tx0: u64, rx1: u64, tx1: u64) -> String {
        format!("{DEV_HEAD}  fict0: {rx0} 1 0 0 0 0 0 0 {tx0} 1 0 0 0 0 0 0\n  fict1: {rx1} 1 0 0 0 0 0 0 {tx1} 1 0 0 0 0 0 0\n")
    }

    fn named_reader(root: &Path, iface: &str) -> Reader {
        let config = ResourcesConfig { show: vec![ResourceItem::Net], net_interface: iface.to_string(), ..ResourcesConfig::default() };
        Reader::new(roots(root, None, refuse), &config)
    }

    #[test]
    fn net_interface_named_skips_the_route_read() {
        let t = named_tree("named");
        assert!(std::fs::read_to_string(t.0.join("proc/net/route")).is_err());
        put(&t.0, "proc/net/dev", &named_dev(5_000, 600, 1_000, 200));
        let mut r = named_reader(&t.0, "fict1");
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).net, None, "the first pass only records");
        put(&t.0, "proc/net/dev", &named_dev(9_000, 700, 4_000, 1_200));
        assert_eq!(r.sample_at(t0 + Duration::from_secs(1)).net, Some(NetRate { down: 3_000, up: 1_000 }));
    }

    #[test]
    fn net_interface_absent_gives_no_figure() {
        let t = named_tree("absent");
        put(&t.0, "proc/net/dev", &named_dev(5_000, 600, 1_000, 200));
        let mut r = named_reader(&t.0, "fict9");
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).net, None);
        put(&t.0, "proc/net/dev", &named_dev(9_000, 700, 4_000, 1_200));
        assert_eq!(r.sample_at(t0 + Duration::from_secs(1)).net, None);
    }

    fn gpu_tree(name: &str) -> Tree {
        let r = std::env::temp_dir().join(format!("alpheus-resources-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        for (f, t) in [
            ("sys/class/hwmon/hwmon0/name", "coretemp\n"),
            ("sys/class/hwmon/hwmon0/temp1_input", "66000\n"),
            ("sys/class/hwmon/hwmon3/name", "amdgpu\n"),
            ("sys/class/hwmon/hwmon3/temp1_label", "edge\n"),
            ("sys/class/hwmon/hwmon3/temp1_input", "54000\n"),
            ("sys/class/hwmon/hwmon3/temp2_label", "junction\n"),
            ("sys/class/hwmon/hwmon3/temp2_input", "61000\n"),
            ("sys/class/hwmon/hwmon5/name", "i915\n"),
            ("sys/class/thermal/thermal_zone2/type", "amdgpu\n"),
            ("sys/class/thermal/thermal_zone2/temp", "70000\n"),
        ] {
            put(&r, f, t);
        }
        Tree(r)
    }

    #[test]
    fn pick_gpu_automatic_or_by_name() {
        let t = gpu_tree("pickgpu");
        assert_eq!(pick_gpu(&t.0, ""), vec![t.0.join("sys/class/hwmon/hwmon3/temp1_input")]);
        assert_eq!(pick_gpu(&t.0, "i915"), Vec::<PathBuf>::new(), "no temperature input");
        assert_eq!(pick_gpu(&t.0, "nouveau"), Vec::<PathBuf>::new());
        assert_eq!(pick_gpu(&t.0, "amdgpu"), vec![t.0.join("sys/class/hwmon/hwmon3/temp1_input")], "never the thermal zone");
    }

    #[test]
    fn the_reader_reads_the_gpu_like_the_cpu() {
        let t = gpu_tree("readgpu");
        let config = ResourcesConfig { show: vec![ResourceItem::Gpu], ..ResourcesConfig::default() };
        let mut r = Reader::new(roots(&t.0, None, refuse), &config);
        let s = r.sample();
        assert_eq!((s.gpu, s.temperature), (Some(54), None));
        std::fs::rename(t.0.join("sys/class/hwmon/hwmon3"), t.0.join("sys/class/hwmon/hwmon7")).unwrap();
        assert_eq!(r.sample().gpu, Some(54), "re-resolved by name");
        std::fs::remove_dir_all(t.0.join("sys/class/hwmon/hwmon7")).unwrap();
        assert_eq!(r.sample().gpu, None, "stale, re-resolved: nothing left");
        assert_eq!(r.sample().gpu, None, "the retry is throttled from here");
        put(&t.0, "sys/class/hwmon/hwmon8/name", "amdgpu\n");
        put(&t.0, "sys/class/hwmon/hwmon8/temp1_input", "50000\n");
        let found_at = (0..RETRY_EMPTY_EVERY_TICKS).find(|_| r.sample().gpu.is_some());
        assert!(found_at.is_some(), "never found again");
        assert_ne!(found_at, Some(0), "not throttled at all");
    }

    /// A fictional `/sys/class/drm`: each `(card, driver)` gets `device/driver` as a link to
    /// `../fictional/drivers/<driver>` (only the link's file name is read) and `device/boot_vga` at 1
    /// (a test about the pick writes 0 where it wants a secondary card), `card0-DP-1` beside them
    /// as a connector never taken for a card, and the workstation's CPU files.
    fn drm_tree(name: &str, cards: &[(&str, &str)]) -> Tree {
        let t = workstation(name);
        std::fs::create_dir_all(t.0.join("sys/class/drm/card0-DP-1")).unwrap();
        for (card, driver) in cards {
            let device = t.0.join("sys/class/drm").join(card).join("device");
            std::fs::create_dir_all(&device).unwrap();
            std::os::unix::fs::symlink(format!("../fictional/drivers/{driver}"), device.join("driver")).unwrap();
            std::fs::write(device.join("boot_vga"), "1\n").unwrap();
        }
        t
    }

    /// An amdgpu card's usage files: busy 37 %, 1 GiB of 8 GiB of VRAM, the card awake.
    fn amdgpu_files(t: &Tree, card: &str) {
        let device = format!("sys/class/drm/{card}/device");
        put(&t.0, &format!("{device}/gpu_busy_percent"), "37\n");
        put(&t.0, &format!("{device}/mem_info_vram_used"), "1073741824\n");
        put(&t.0, &format!("{device}/mem_info_vram_total"), "8589934592\n");
        put(&t.0, &format!("{device}/power/runtime_status"), "active\n");
    }

    /// The amdgpu hwmon temperature, 54 °C.
    fn amdgpu_hwmon(t: &Tree) {
        put(&t.0, "sys/class/hwmon/hwmon3/name", "amdgpu\n");
        put(&t.0, "sys/class/hwmon/hwmon3/temp1_input", "54000\n");
    }

    fn gpu_reader(root: &Path, metric: GpuMetric) -> Reader {
        let config = ResourcesConfig { show: vec![ResourceItem::Gpu], gpu_metric: metric, ..ResourcesConfig::default() };
        Reader::new(roots(root, None, refuse), &config)
    }

    const BUSY_37: Option<GpuBusy> = Some(GpuBusy { percent: 37, idle_based: false });

    #[test]
    fn gpu_usage_reads_amdgpu_busy_and_vram() {
        let t = drm_tree("amdbusy", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        let card = pick_card(&t.0, "").expect("the amdgpu card");
        assert_eq!((card.dir.clone(), card.driver), (t.0.join("sys/class/drm/card1"), GpuDriver::Amdgpu));
        assert_eq!(read_amdgpu(&card), (Some(37), Some(13)), "1 GiB of 8 GiB is 12.5 %, rounded up");
        let s = gpu_reader(&t.0, GpuMetric::Usage).sample();
        assert_eq!((s.gpu_busy, s.vram, s.gpu), (BUSY_37, Some(13), None));
    }

    #[test]
    fn a_monitor_only_card_never_wins() {
        let t = drm_tree("evdi", &[("card0", "evdi"), ("card1", "amdgpu")]);
        assert_eq!(pick_card(&t.0, "").map(|c| c.dir), Some(t.0.join("sys/class/drm/card1")));
        assert_eq!(pick_card(&t.0, "amdgpu").map(|c| c.dir), Some(t.0.join("sys/class/drm/card1")), "by name");
        assert!(pick_card(&t.0, "evdi").is_none(), "not even by name");
        assert!(pick_card(&t.0, "i915").is_none(), "a hint names that driver only");
        let t = drm_tree("evdialone", &[("card0", "evdi")]);
        assert!(pick_card(&t.0, "").is_none());
    }

    #[test]
    fn gpu_nvidia_proprietary_gives_nothing_and_no_panic() {
        let t = drm_tree("nvidia", &[("card0", "nvidia")]);
        assert!(pick_card(&t.0, "").is_none());
        for metric in [GpuMetric::Auto, GpuMetric::Usage, GpuMetric::Both, GpuMetric::Temp] {
            let mut r = gpu_reader(&t.0, metric);
            for pass in 0..3 {
                let s = r.sample();
                assert_eq!((s.gpu_busy, s.vram, s.gpu), (None, None, None), "{metric:?}, pass {pass}");
            }
        }
    }

    #[test]
    fn a_suspended_gpu_is_not_read() {
        let t = drm_tree("suspended", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "suspended\n");
        let card = pick_card(&t.0, "").unwrap();
        assert!(!runtime_active(&card));
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let s = r.sample();
        assert_eq!((s.gpu_busy, s.vram), (None, None), "the busy file holds 37, yet a suspended card is not read");
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "active\n");
        assert!(runtime_active(&card));
        let s = r.sample();
        assert_eq!((s.gpu_busy, s.vram), (BUSY_37, Some(13)));
    }

    #[test]
    fn gpu_metric_temp_reads_no_usage_file() {
        let t = drm_tree("temponly", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        amdgpu_hwmon(&t);
        let s = gpu_reader(&t.0, GpuMetric::Temp).sample();
        assert_eq!((s.gpu, s.gpu_busy, s.vram), (Some(54), None, None));
        let s = gpu_reader(&t.0, GpuMetric::Both).sample();
        assert_eq!((s.gpu, s.gpu_busy, s.vram), (Some(54), BUSY_37, Some(13)), "both reads both");
    }

    #[test]
    fn gpu_metric_auto_prefers_usage_and_skips_the_sensor() {
        let t = drm_tree("auto", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        amdgpu_hwmon(&t);
        let s = gpu_reader(&t.0, GpuMetric::Auto).sample();
        assert_eq!((s.gpu_busy, s.gpu), (BUSY_37, None));
        // No card at start: the sensor instead, as before usage existed.
        let t = drm_tree("autonocard", &[]);
        amdgpu_hwmon(&t);
        let s = gpu_reader(&t.0, GpuMetric::Auto).sample();
        assert_eq!((s.gpu_busy, s.gpu), (None, Some(54)));
    }

    #[test]
    fn a_vanished_card_is_picked_again_throttled() {
        let t = drm_tree("vanished", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        assert_eq!(r.sample().gpu_busy, BUSY_37);
        let card = t.0.join("sys/class/drm/card1");
        std::fs::remove_dir_all(&card).unwrap();
        assert_eq!(r.sample().gpu_busy, None, "gone: picked again, nothing found");
        std::fs::create_dir_all(card.join("device")).unwrap();
        std::os::unix::fs::symlink("../fictional/drivers/amdgpu", card.join("device/driver")).unwrap();
        std::fs::write(card.join("device/boot_vga"), "1\n").unwrap();
        amdgpu_files(&t, "card1");
        let found_at = (0..=RETRY_EMPTY_EVERY_TICKS).find(|_| r.sample().gpu_busy.is_some());
        assert!(found_at.is_some(), "never picked again");
        assert_ne!(found_at, Some(0), "picked again on the very next tick: not throttled at all");
    }

    #[test]
    fn auto_picks_only_the_boot_vga_card() {
        // A hybrid laptop: the integrated GPU drives the panel, the discrete one sleeps.
        let t = drm_tree("bootvga", &[("card0", "i915"), ("card1", "amdgpu")]);
        put(&t.0, "sys/class/drm/card1/device/boot_vga", "0\n");
        assert_eq!(pick_card(&t.0, "").map(|c| c.dir), Some(t.0.join("sys/class/drm/card0")), "amdgpu is preferred, but not boot_vga");
        assert_eq!(pick_card(&t.0, "amdgpu").map(|c| c.dir), Some(t.0.join("sys/class/drm/card1")), "named: read anyway");
        let t = drm_tree("bootvgaalone", &[("card1", "amdgpu")]);
        put(&t.0, "sys/class/drm/card1/device/boot_vga", "0\n");
        assert!(pick_card(&t.0, "").is_none());
        std::fs::remove_file(t.0.join("sys/class/drm/card1/device/boot_vga")).unwrap();
        assert!(pick_card(&t.0, "").is_none(), "no boot_vga file: not the boot card");
    }

    #[test]
    fn only_an_active_card_is_read() {
        let t = drm_tree("states", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        let status = "sys/class/drm/card1/device/power/runtime_status";
        for (state, read) in [("suspending", false), ("resuming", false), ("suspended", false), ("active", true), ("unsupported", true)] {
            put(&t.0, status, &format!("{state}\n"));
            let s = gpu_reader(&t.0, GpuMetric::Usage).sample();
            assert_eq!(s.gpu_busy.is_some(), read, "{state}");
        }
        std::fs::remove_file(t.0.join(status)).unwrap();
        assert_eq!(gpu_reader(&t.0, GpuMetric::Usage).sample().gpu_busy, BUSY_37, "no runtime PM: read");
    }

    /// An awake amdgpu card with a 10 s autosuspend delay, busy 37 %.
    fn throttled_tree(name: &str) -> Tree {
        let t = drm_tree(name, &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        put(&t.0, "sys/class/drm/card1/device/power/autosuspend_delay_ms", "10000\n");
        t
    }

    fn busy(percent: u8) -> Option<GpuBusy> {
        Some(GpuBusy { percent, idle_based: false })
    }

    #[test]
    fn usage_is_read_once_per_autosuspend_delay_and_a_second() {
        let t = throttled_tree("spaced");
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).gpu_busy, BUSY_37);
        put(&t.0, "sys/class/drm/card1/device/gpu_busy_percent", "50\n");
        let s = r.sample_at(t0 + Duration::from_secs(10));
        assert_eq!((s.gpu_busy, s.vram), (BUSY_37, Some(13)), "not read before the delay and a second: the last figures repeat");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(11)).gpu_busy, busy(50));
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "suspended\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(30)).gpu_busy, None, "asleep: nothing, nothing repeated");
    }

    #[test]
    fn both_skips_the_sensor_of_a_sleeping_card() {
        let t = throttled_tree("bothasleep");
        amdgpu_hwmon(&t);
        let mut r = gpu_reader(&t.0, GpuMetric::Both);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).gpu, Some(54));
        put(&t.0, "sys/class/hwmon/hwmon3/temp1_input", "60000\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu, Some(54), "throttled: the temperature repeats too");
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "suspended\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(30)).gpu, None, "asleep: the hwmon is not read either");
    }

    #[test]
    fn no_autosuspend_delay_reads_every_pass() {
        let t = drm_tree("nodelay", &[("card1", "amdgpu")]);
        amdgpu_files(&t, "card1");
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).gpu_busy, BUSY_37);
        put(&t.0, "sys/class/drm/card1/device/gpu_busy_percent", "50\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu_busy, busy(50), "no file: runtime PM off, never asleep");
        let t = throttled_tree("negdelay");
        put(&t.0, "sys/class/drm/card1/device/power/autosuspend_delay_ms", "-1\n");
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        assert_eq!(r.sample_at(t0).gpu_busy, BUSY_37);
        put(&t.0, "sys/class/drm/card1/device/gpu_busy_percent", "50\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(1)).gpu_busy, busy(50), "a negative delay: autosuspend off");
    }

    #[test]
    fn temp_spaces_the_hwmon_of_a_named_secondary_card() {
        let t = throttled_tree("tempsecondary");
        amdgpu_hwmon(&t);
        put(&t.0, "sys/class/drm/card1/device/boot_vga", "0\n");
        let config = ResourcesConfig {
            show: vec![ResourceItem::Gpu],
            gpu_metric: GpuMetric::Temp,
            gpu_sensor: "amdgpu".into(),
            ..ResourcesConfig::default()
        };
        let mut r = Reader::new(roots(&t.0, None, refuse), &config);
        let t0 = Instant::now();
        let s = r.sample_at(t0);
        assert_eq!((s.gpu, s.gpu_busy, s.vram), (Some(54), None, None), "no usage file read in temp");
        put(&t.0, "sys/class/hwmon/hwmon3/temp1_input", "60000\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu, Some(54), "spaced like a usage read");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(11)).gpu, Some(60));
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "suspended\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(30)).gpu, None, "asleep: not read");
        // The boot card's hwmon in temp: every pass, as before.
        put(&t.0, "sys/class/drm/card1/device/boot_vga", "1\n");
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "active\n");
        let mut r = Reader::new(roots(&t.0, None, refuse), &config);
        assert_eq!(r.sample_at(t0).gpu, Some(60));
        put(&t.0, "sys/class/hwmon/hwmon3/temp1_input", "61000\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu, Some(61));
    }

    /// An i915 card (the laptop's integrated GPU, `gt/gt0/rc6_residency_ms` as listed on 2026-11-27)
    /// awake, its RC6 residency at `rc6` ms, no autosuspend delay (read every pass).
    fn i915_tree(name: &str, rc6: &str) -> Tree {
        let t = drm_tree(name, &[("card1", "i915")]);
        put(&t.0, "sys/class/drm/card1/device/power/runtime_status", "active\n");
        put(&t.0, "sys/class/drm/card1/gt/gt0/rc6_residency_ms", rc6);
        t
    }

    const ESTIMATE_25: Option<GpuBusy> = Some(GpuBusy { percent: 25, idle_based: true });

    #[test]
    fn gpu_usage_from_rc6_is_one_minus_the_idle_share() {
        let t = i915_tree("rc6", "1000\n");
        let card = pick_card(&t.0, "").expect("the i915 card");
        assert_eq!(idle_ms(&card), Some(1000));
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let t0 = Instant::now();
        let s = r.sample_at(t0);
        assert_eq!((s.gpu_busy, s.vram), (None, None), "the first pass has no earlier reading, like the CPU");
        put(&t.0, "sys/class/drm/card1/gt/gt0/rc6_residency_ms", "2500\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu_busy, ESTIMATE_25, "idle 1.5 s of 2 s");
    }

    #[test]
    fn busy_from_idle_clamps_and_refuses_a_counter_going_back() {
        let t0 = Instant::now();
        let t2 = t0 + Duration::from_secs(2);
        assert_eq!(busy_from_idle((1000, t0), (2500, t2)), Some(25));
        assert_eq!(busy_from_idle((1000, t0), (1000, t2)), Some(100), "never idle");
        assert_eq!(busy_from_idle((1000, t0), (1333, t0 + Duration::from_secs(1))), Some(67), "rounded");
        assert_eq!(busy_from_idle((1000, t0), (3500, t2)), Some(0), "idle longer than the interval: clamped");
        assert_eq!(busy_from_idle((2500, t0), (1000, t2)), None, "the counter went back");
        assert_eq!(busy_from_idle((1000, t0), (1500, t0)), None, "no time elapsed");
    }

    #[test]
    fn the_older_rc6_path_is_read_when_gt0_is_absent() {
        let t = drm_tree("rc6old", &[("card1", "i915")]);
        put(&t.0, "sys/class/drm/card1/power/rc6_residency_ms", "4200\n");
        let card = pick_card(&t.0, "").unwrap();
        assert_eq!(idle_ms(&card), Some(4200));
        put(&t.0, "sys/class/drm/card1/gt/gt0/rc6_residency_ms", "7\n");
        assert_eq!(idle_ms(&card), Some(7), "gt0 first when both exist");
    }

    #[test]
    fn a_suspension_drops_the_idle_baseline() {
        let t = i915_tree("rc6suspend", "1000\n");
        let status = "sys/class/drm/card1/device/power/runtime_status";
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).gpu_busy, None);
        put(&t.0, status, "suspended\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu_busy, None, "asleep: not read");
        put(&t.0, status, "active\n");
        put(&t.0, "sys/class/drm/card1/gt/gt0/rc6_residency_ms", "9000\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(10)).gpu_busy, None, "a new baseline, no difference across the sleep");
        put(&t.0, "sys/class/drm/card1/gt/gt0/rc6_residency_ms", "10500\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(12)).gpu_busy, ESTIMATE_25);
    }

    #[test]
    fn xe_idle_residency_is_read_from_the_tile_tree() {
        // Not seen on hardware: the tree follows the kernel's xe driver (xe_tile_sysfs.c adds
        // `tile<N>` under the PCI device, xe_gt_sysfs.c `gt<N>` under it, xe_gt_idle.c `gtidle`
        // with `idle_residency_ms`).
        let t = drm_tree("xe", &[("card0", "xe")]);
        put(&t.0, "sys/class/drm/card0/device/power/runtime_status", "active\n");
        let file = "sys/class/drm/card0/device/tile0/gt0/gtidle/idle_residency_ms";
        put(&t.0, file, "1000\n");
        let card = pick_card(&t.0, "").unwrap();
        assert_eq!((card.driver, idle_ms(&card)), (GpuDriver::Xe, Some(1000)));
        let mut r = gpu_reader(&t.0, GpuMetric::Usage);
        let t0 = Instant::now();
        assert_eq!(r.sample_at(t0).gpu_busy, None);
        put(&t.0, file, "2500\n");
        assert_eq!(r.sample_at(t0 + Duration::from_secs(2)).gpu_busy, ESTIMATE_25);
    }
}
