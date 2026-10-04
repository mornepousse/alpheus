//! Resources, read from /proc and /sys with no command at all, on the event
//! loop (a few µs per reading, every `[resources] interval` seconds): CPU use
//! since the previous reading (the aggregate `cpu` line only, so CPU hotplug
//! never confuses it), memory in use (tenths of GiB, falling back to
//! `MemFree` when `MemAvailable` is absent), the chosen sensor's
//! temperature — the max across every match when several packages or
//! drivers share the hint's name, re-resolved by name whenever a cached
//! sensor file stops existing (hwmon numbers are not stable across a reboot
//! or a hot-(re)plug: never kept as a bare `hwmonN`). A file that can't be
//! read leaves its own figure out; the next tick tries again (resources
//! have no down state). `root` is "/" in the daemon, a temporary tree in
//! tests.

use crate::system::Sample;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

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

/// Memory in use, MemTotal − MemAvailable (MemFree on a kernel without
/// MemAvailable, pre-3.14), in tenths of GiB, rounded (Q5: shown "10.5G").
pub fn parse_meminfo(text: &str) -> Option<u16> {
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok());
    let total = field("MemTotal:").filter(|t| *t > 0)?;
    let available = field("MemAvailable:").or_else(|| field("MemFree:"))?;
    let used = total.saturating_sub(available);
    u16::try_from((used * 10 + GIB_KB / 2) / GIB_KB).ok()
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

/// Every hwmon or thermal-zone file matching `hint` (or, empty, the first
/// name found by preference) — several when more than one package or driver
/// shares that name; the caller takes the max.
pub fn pick_sensor(root: &Path, hint: &str) -> Vec<PathBuf> {
    let hwmon: Vec<(String, PathBuf)> = entries(&root.join("sys/class/hwmon"))
        .into_iter()
        .filter_map(|d| Some((first_line(&d.join("name"))?, hwmon_temp(&d))))
        .filter(|(_, p)| p.exists())
        .collect();
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

pub struct Reader {
    root: PathBuf,
    hint: String,
    sensors: Vec<PathBuf>,
    previous: Option<CpuTimes>,
}

impl Reader {
    /// Resolves the sensors once, by name, at startup.
    pub fn new(root: &Path, hint: &str) -> Reader {
        Reader { root: root.to_path_buf(), hint: hint.to_string(), sensors: pick_sensor(root, hint), previous: None }
    }

    /// The first match, diagnostics only: `sample` reads every match and
    /// keeps the max.
    pub fn sensor(&self) -> Option<&Path> {
        self.sensors.first().map(PathBuf::as_path)
    }

    /// The max reading across every match; re-resolves by name first when a
    /// cached path no longer exists (hwmon is not stable across a reboot or
    /// a hot-(re)plug — never addressed as a bare `hwmonN`).
    fn temperature(&mut self) -> Option<i16> {
        if self.sensors.iter().any(|p| !p.exists()) {
            self.sensors = pick_sensor(&self.root, &self.hint);
        }
        self.sensors.iter().filter_map(|p| read_temperature(p)).max()
    }

    pub fn sample(&mut self) -> Sample {
        let now = first_line(&self.root.join("proc/stat")).and_then(|l| parse_stat(&l));
        let cpu = match (self.previous, now) {
            (Some(a), Some(b)) => cpu_percent(a, b),
            _ => None,
        };
        self.previous = now;
        let memory = std::fs::read_to_string(self.root.join("proc/meminfo")).ok().and_then(|t| parse_meminfo(&t));
        Sample { cpu, memory, temperature: self.temperature() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_A: &str = include_str!("../../tests/fixtures/proc-stat-a.txt");
    const STAT_B: &str = include_str!("../../tests/fixtures/proc-stat-b.txt");
    const MEMINFO: &str = include_str!("../../tests/fixtures/proc-meminfo.txt");

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
    fn memory_in_use_from_memavailable() {
        assert_eq!(parse_meminfo(MEMINFO), Some(105), "10.5 GiB in use");
        assert_eq!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn memory_falls_back_to_memfree_without_memavailable() {
        // Pre-3.14 kernels have no MemAvailable at all; same fixture, that
        // line dropped — matches the 217 already worked out in step 6.
        let no_available: String = MEMINFO.lines().filter(|l| !l.starts_with("MemAvailable:")).collect::<Vec<_>>().join("\n");
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
        let mut reader = Reader::new(&t.0, "");
        assert_eq!(reader.sample().temperature, Some(71), "the hotter package");
    }

    #[test]
    fn the_reader_first_without_cpu_then_38() {
        let t = workstation("reader");
        let mut r = Reader::new(&t.0, "");
        assert_eq!(r.sample(), Sample { cpu: None, memory: Some(105), temperature: Some(66) });
        put(&t.0, "proc/stat", STAT_B);
        assert_eq!(r.sample(), Sample { cpu: Some(38), memory: Some(105), temperature: Some(66) });
        put(&t.0, "sys/class/hwmon/hwmon9/temp1_input", "-5000\n");
        assert_eq!(r.sample().temperature, Some(-5));
    }

    #[test]
    fn hwmon_renumbering_is_not_cached_across_a_reload() {
        let t = workstation("renumber");
        let mut r = Reader::new(&t.0, "");
        assert_eq!(r.sample().temperature, Some(66), "hwmon9, coretemp");
        // Never a boot-stable number: the same sensor comes back under a
        // different hwmonN (a reboot, or a dock's hwmon shifting the index).
        std::fs::rename(t.0.join("sys/class/hwmon/hwmon9"), t.0.join("sys/class/hwmon/hwmon2")).unwrap();
        assert_eq!(r.sample().temperature, Some(66), "re-resolved by name, not the stale hwmon9 path");
    }

    #[test]
    fn nothing_readable_gives_an_empty_sample() {
        let mut r = Reader::new(Path::new("/nonexistent"), "");
        assert_eq!(r.sensor(), None);
        assert_eq!(r.sample(), Sample::default());
    }
}
