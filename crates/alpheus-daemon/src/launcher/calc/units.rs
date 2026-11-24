//! The calculator's units: one static table, each entry with its aliases (lower case,
//! matching is case-insensitive), its symbol for output, and a factor and offset to the
//! dimension's base (m, kg, K, B, s): `base = value * factor + offset`.

use super::CalcError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dimension {
    Length,
    Mass,
    Temperature,
    Data,
    Time,
}

impl Dimension {
    fn name(self) -> &'static str {
        match self {
            Length => "length",
            Mass => "mass",
            Temperature => "temperature",
            Data => "data size",
            Time => "time",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Unit {
    pub symbol: &'static str,
    pub dimension: Dimension,
    factor: f64,
    offset: f64,
}

const fn u(symbol: &'static str, dimension: Dimension, factor: f64) -> Unit {
    Unit { symbol, dimension, factor, offset: 0.0 }
}

use Dimension::{Data, Length, Mass, Temperature, Time};

static TABLE: &[(&[&str], Unit)] = &[
    (&["mm"], u("mm", Length, 0.001)),
    (&["cm"], u("cm", Length, 0.01)),
    (&["m"], u("m", Length, 1.0)),
    (&["km"], u("km", Length, 1000.0)),
    (&["inch", "inches"], u("in", Length, 0.0254)),
    (&["ft", "foot", "feet"], u("ft", Length, 0.3048)),
    (&["yd"], u("yd", Length, 0.9144)),
    (&["mi", "mile", "miles"], u("mi", Length, 1609.344)),
    (&["mg"], u("mg", Mass, 1e-6)),
    (&["g"], u("g", Mass, 0.001)),
    (&["kg"], u("kg", Mass, 1.0)),
    (&["t", "tonne"], u("t", Mass, 1000.0)),
    (&["oz"], u("oz", Mass, 0.028_349_523_125)),
    (&["lb", "lbs"], u("lb", Mass, 0.453_592_37)),
    (&["c", "°c", "degc", "celsius"], Unit { symbol: "°C", dimension: Temperature, factor: 1.0, offset: 273.15 }),
    (&["f", "°f", "degf", "fahrenheit"], Unit { symbol: "°F", dimension: Temperature, factor: 5.0 / 9.0, offset: 459.67 * 5.0 / 9.0 }),
    (&["k", "kelvin"], u("K", Temperature, 1.0)),
    (&["b"], u("B", Data, 1.0)),
    (&["kb"], u("KB", Data, 1e3)),
    (&["mb"], u("MB", Data, 1e6)),
    (&["gb"], u("GB", Data, 1e9)),
    (&["tb"], u("TB", Data, 1e12)),
    (&["pb"], u("PB", Data, 1e15)),
    (&["kib"], u("KiB", Data, 1024.0)),
    (&["mib"], u("MiB", Data, 1_048_576.0)),
    (&["gib"], u("GiB", Data, 1_073_741_824.0)),
    (&["tib"], u("TiB", Data, 1_099_511_627_776.0)),
    (&["pib"], u("PiB", Data, 1_125_899_906_842_624.0)),
    (&["ms"], u("ms", Time, 0.001)),
    (&["s", "sec"], u("s", Time, 1.0)),
    (&["min"], u("min", Time, 60.0)),
    (&["h", "hr"], u("h", Time, 3600.0)),
    (&["d", "day", "days"], u("d", Time, 86_400.0)),
    (&["wk", "week", "weeks"], u("wk", Time, 604_800.0)),
];

/// The unit called `name`, in any case.
pub(crate) fn unit(name: &str) -> Option<&'static Unit> {
    let name = name.to_lowercase();
    TABLE.iter().find(|(aliases, _)| aliases.contains(&name.as_str())).map(|(_, unit)| unit)
}

pub(crate) fn convert(value: f64, from: &Unit, to: &Unit) -> Result<f64, CalcError> {
    if from.dimension != to.dimension {
        return Err(CalcError::Units(format!("cannot convert {} to {}", from.dimension.name(), to.dimension.name())));
    }
    let result = (value * from.factor + from.offset - to.offset) / to.factor;
    if result.is_nan() {
        Err(CalcError::Domain)
    } else if result.is_infinite() {
        Err(CalcError::Overflow)
    } else {
        Ok(result)
    }
}
