//! Table cells that print the way the Python scripts' CSV writer did, so the Rust output can be
//! compared byte for byte with the committed result files.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Largest table, JSON or log the bench reads whole (64 MiB: far above any real run).
pub const MAX_TEXT: u64 = 64 << 20;
/// Largest picture the bench reads (16 MiB).
pub const MAX_PICTURE: u64 = 16 << 20;
/// Largest ELF `elf2uf2` reads (32 MiB).
pub const MAX_ELF: u64 = 32 << 20;

/// Read a whole file, but not more than `max` bytes: a bigger one is an error that names the
/// file and both sizes, not an out-of-memory kill.
pub fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    if buf.len() as u64 > max {
        bail!(
            "{} is larger than the {max} bytes the bench reads",
            path.display()
        );
    }
    Ok(buf)
}

/// [`read_text`], or `None` when the file is not there.
pub fn read_text_opt(path: &Path, max: u64) -> Result<Option<String>> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => read_text(path, max).map(Some),
    }
}

/// [`read_capped`] as UTF-8 text.
pub fn read_text(path: &Path, max: u64) -> Result<String> {
    String::from_utf8(read_capped(path, max)?)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", path.display()))
}

/// One CSV cell: empty, an integer, a float, a bool or text.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    None,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Cell {
    pub fn text(&self) -> String {
        match self {
            Cell::None => String::new(),
            Cell::Int(i) => i.to_string(),
            // Rust's Debug prints the shortest round-trip form with a ".0" on whole numbers, like repr().
            Cell::Float(f) => format!("{f:?}"),
            Cell::Bool(true) => "True".into(),
            Cell::Bool(false) => "False".into(),
            Cell::Str(s) => s.clone(),
        }
    }

    pub fn from_json(v: &Value) -> Result<Cell> {
        Ok(match v {
            Value::Null => Cell::None,
            Value::Bool(b) => Cell::Bool(*b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Cell::Int(i),
                None => Cell::Float(n.as_f64().context("number out of range")?),
            },
            Value::String(s) => Cell::Str(s.clone()),
            other => bail!("not a table value: {other}"),
        })
    }
}

impl From<Option<f64>> for Cell {
    fn from(v: Option<f64>) -> Cell {
        v.map_or(Cell::None, Cell::Float)
    }
}

/// Python's `round(x, nd)`: the correctly rounded decimal of the exact binary value.
pub fn round_to(x: f64, nd: usize) -> f64 {
    format!("{x:.nd$}")
        .parse()
        .expect("a formatted float parses")
}

/// Python's `float(s)` as the scripts used it (`None` when it is not a number).
pub fn num(s: &str) -> Option<f64> {
    s.trim().parse().ok().filter(|v: &f64| v.is_finite())
}

/// The `mean` of the analysis: ignores missing values, rounds to 2 places.
pub fn mean(v: &[Option<f64>]) -> Option<f64> {
    let v: Vec<f64> = v.iter().flatten().copied().collect();
    (!v.is_empty()).then(|| round_to(v.iter().sum::<f64>() / v.len() as f64, 2))
}

/// Smallest and largest present value.
pub fn rng(v: &[Option<f64>]) -> (Option<f64>, Option<f64>) {
    let v: Vec<f64> = v.iter().flatten().copied().collect();
    if v.is_empty() {
        return (None, None);
    }
    (
        Some(v.iter().copied().fold(f64::INFINITY, f64::min)),
        Some(v.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
    )
}

/// Python's `%g`-style `format(x, "g")`: six significant digits, no trailing zeros.
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return x.to_string();
    }
    let exp = format!("{x:.5e}");
    let Some((mant, e)) = exp.split_once('e') else {
        return x.to_string();
    };
    let Ok(e) = e.parse::<i32>() else {
        return x.to_string();
    };
    if (-4..6).contains(&e) {
        let s = format!("{:.*}", (5 - e).max(0) as usize, x);
        if s.contains('.') {
            return s.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        return s;
    }
    let mant = if mant.contains('.') {
        mant.trim_end_matches('0').trim_end_matches('.')
    } else {
        mant
    };
    format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

/// Write a CSV the way Python's `csv.writer` does (CRLF line ends).
pub fn write_csv(path: &Path, header: &[&str], rows: &[Vec<Cell>]) -> Result<()> {
    let mut w = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_path(path)
        .with_context(|| format!("create {}", path.display()))?;
    w.write_record(header)?;
    for r in rows {
        w.write_record(r.iter().map(Cell::text))?;
    }
    w.flush()?;
    Ok(())
}

/// Read a CSV into one map per row, keyed by the header (Python's `DictReader`).
pub fn read_dict(path: &Path) -> Result<Vec<HashMap<String, String>>> {
    let bytes = read_capped(path, MAX_TEXT)?;
    let mut r = csv::Reader::from_reader(bytes.as_slice());
    let head = r.headers()?.clone();
    let mut out = Vec::new();
    for rec in r.records() {
        let rec = rec.with_context(|| format!("read {}", path.display()))?;
        out.push(
            head.iter()
                .zip(rec.iter())
                .map(|(h, v)| (h.to_string(), v.to_string()))
                .collect(),
        );
    }
    Ok(out)
}

/// A table field, or an error naming the file when the column is missing.
pub fn field<'a>(row: &'a HashMap<String, String>, key: &str, path: &Path) -> Result<&'a str> {
    row.get(key)
        .map(String::as_str)
        .with_context(|| format!("{}: no column {key}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_rounding_and_repr() {
        assert_eq!(round_to(2.675, 2), 2.67); // the binary value is below the tie, as in Python
        assert_eq!(round_to(0.125, 2), 0.12); // exact tie goes to even
        assert_eq!(Cell::Float(5.0).text(), "5.0");
        assert_eq!(Cell::Float(8.33).text(), "8.33");
        assert_eq!(Cell::Bool(true).text(), "True");
        assert_eq!(Cell::None.text(), "");
    }

    #[test]
    fn g_format() {
        assert_eq!(fmt_g(0.0), "0");
        assert_eq!(fmt_g(13.674), "13.674");
        assert_eq!(fmt_g(100.0), "100");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(0.00001234), "1.234e-05");
        assert_eq!(fmt_g(12.5), "12.5");
        assert_eq!(fmt_g(f64::NAN), "NaN");
        assert_eq!(fmt_g(f64::INFINITY), "inf");
        assert_eq!(fmt_g(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn num_refuses_non_finite() {
        for s in [
            "nan", "NaN", "inf", "-inf", "infinity", "1e999", "", "x", "1,5",
        ] {
            assert_eq!(num(s), None, "{s}");
        }
        assert_eq!(num(" 1.5 "), Some(1.5));
    }

    #[test]
    fn mean_ignores_missing() {
        assert_eq!(mean(&[Some(1.0), None, Some(2.0)]), Some(1.5));
        assert_eq!(mean(&[None]), None);
        assert_eq!(rng(&[Some(3.0), None, Some(-1.0)]), (Some(-1.0), Some(3.0)));
    }

    #[test]
    fn random_and_mutated_csv_never_panics() {
        let d = crate::testutil::Scratch::new("cell-fuzz");
        let p = d.0.join("x.csv");
        let mut rng = crate::testutil::Rng(7);
        let seed = b"a,b,c\r\n1,2.5,nan\r\n\"q,\"\"x\",inf,-1e999\r\n";
        for i in 0..2000 {
            let v: Vec<u8> = if i % 2 == 0 {
                rng.mutate(seed)
            } else {
                (0..rng.below(200)).map(|_| rng.next() as u8).collect()
            };
            std::fs::write(&p, v).unwrap();
            if let Ok(rows) = read_dict(&p) {
                for r in rows {
                    for v in r.values() {
                        if let Some(x) = num(v) {
                            let _ = fmt_g(x);
                            let _ = round_to(x, 2);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fmt_g_never_panics() {
        let mut rng = crate::testutil::Rng(9);
        for _ in 0..20000 {
            let _ = fmt_g(f64::from_bits(rng.next()));
        }
    }

    #[test]
    fn reads_are_bounded() {
        let d = crate::testutil::Scratch::new("cell-cap");
        let p = d.0.join("big");
        std::fs::write(&p, vec![b'x'; 1001]).unwrap();
        let e = read_capped(&p, 1000).unwrap_err().to_string();
        assert!(e.contains("larger than") && e.contains("big"), "{e}");
        assert_eq!(read_capped(&p, 1001).unwrap().len(), 1001);
        std::fs::write(&p, [0xffu8, 0xfe]).unwrap();
        assert!(read_text(&p, 10).is_err());
        assert!(read_text_opt(&d.0.join("none"), 10).unwrap().is_none());
        assert!(read_dict(&d.0.join("none")).is_err());
    }
}
