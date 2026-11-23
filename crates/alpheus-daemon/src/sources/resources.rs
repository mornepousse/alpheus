//! Resources, read from /proc and /sys with no command at all, on the event
//! loop (a few µs per reading, every `[resources] interval` seconds): CPU use
//! since the previous reading (the aggregate `cpu` line only, so CPU hotplug
//! never confuses it), memory in use like htop (tenths of GiB, caches left
//! out, shared memory kept; see `parse_meminfo`), the chosen sensor's
//! temperature — the max across every match when several packages or
//! drivers share the hint's name, re-resolved by name whenever a cached
//! sensor file stops existing (hwmon numbers are not stable across a reboot
//! or a hot-(re)plug: never kept as a bare `hwmonN`). A file that can't be
//! read leaves its own figure out; the next tick tries again (resources
//! have no down state). `root` is "/" in the daemon, a temporary tree in
//! tests.

use crate::config::{ResourceItem, ResourcesConfig};
use crate::log::write as log;
use crate::sys::FsBlocks;
use crate::system::{NetRate, Sample};
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

/// Memory in use, in tenths of GiB, rounded (Q5: shown "10.5G"). Like htop
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
    previous: Option<CpuTimes>,
    /// The interface of the last successful net reading, its counters and when.
    net: Option<(String, NetCounters, Instant)>,
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
        let gpu_sensor = sensor(ResourceItem::Gpu, SensorKind::Gpu, &config.gpu_sensor);
        Reader { roots, show, cpu_sensor, gpu_sensor, previous: None, net: None }
    }

    fn shows(&self, item: ResourceItem) -> bool {
        self.show.contains(&item)
    }

    /// The first temperature match, diagnostics only: `sample` reads every match and keeps the max.
    pub fn sensor(&self) -> Option<&Path> {
        self.cpu_sensor.as_ref()?.sensors.first().map(|(p, _)| p.as_path())
    }

    /// Net throughput of the default route's interface against the last reading of that same
    /// interface; the new counters replace the old ones on every successful read.
    fn net(&mut self, now: Instant) -> Option<NetRate> {
        let route = std::fs::read_to_string(self.roots.root.join("proc/net/route")).ok().and_then(|t| default_route(&t));
        let Some(iface) = route else {
            self.net = None;
            return None;
        };
        let counters = std::fs::read_to_string(self.roots.root.join("proc/net/dev")).ok().and_then(|t| net_counters(&t, &iface));
        let Some(counters) = counters else {
            self.net = None;
            return None;
        };
        let rate = match &self.net {
            Some((was, prev, at)) if *was == iface => net_rate(*prev, counters, now.saturating_duration_since(*at)),
            _ => None,
        };
        self.net = Some((iface, counters, now));
        rate
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
        if self.shows(ResourceItem::Memory) {
            if let Ok(text) = std::fs::read_to_string(self.roots.root.join("proc/meminfo")) {
                sample.memory = parse_meminfo(&text);
                sample.memory_total = parse_mem_total(&text);
            }
        }
        if self.shows(ResourceItem::Temperature) {
            sample.temperature = self.cpu_sensor.as_mut().and_then(|s| s.read(&self.roots.root));
        }
        if self.shows(ResourceItem::Disk) {
            // A failing call leaves the figure out; the next tick asks again.
            sample.disk = self.roots.home.as_deref().and_then(self.roots.statfs).and_then(disk_percent);
        }
        if self.shows(ResourceItem::Net) {
            sample.net = self.net(now);
        }
        if self.shows(ResourceItem::Gpu) {
            sample.gpu = self.gpu_sensor.as_mut().and_then(|s| s.read(&self.roots.root));
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
        // Pre-3.14 kernels have no MemAvailable at all; same fixture, that
        // line (and the cache lines) dropped — matches the 217 already worked out in step 6.
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
    fn the_disk_of_home_through_the_injected_statfs() {
        let t = workstation("disk");
        assert_eq!(disk_reader(&t.0, Some("/fictional/home"), fake).sample().disk, Some(62));
        assert_eq!(disk_reader(&t.0, None, fake).sample().disk, None, "no $HOME, no figure");
    }

    #[test]
    fn a_failing_statfs_hides_the_disk_and_is_retried() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn flaky(_: &Path) -> Option<FsBlocks> {
            (CALLS.fetch_add(1, Ordering::SeqCst) > 0).then_some(FsBlocks { blocks: 1000, free: 380, avail: 380 })
        }
        let t = workstation("flaky");
        let mut r = disk_reader(&t.0, Some("/fictional/home"), flaky);
        assert_eq!(r.sample().disk, None);
        assert_eq!(r.sample().disk, Some(62));
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
        assert_eq!(d.sample().disk, None);
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
}
