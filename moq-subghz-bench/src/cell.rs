//! Table cells that print the way the Python scripts' CSV writer did, so the Rust output can be
//! compared byte for byte with the committed result files.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

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
    s.trim().parse().ok()
}

/// Python's `mean` helper of analyze-051.py: ignores missing values, rounds to 2 places.
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
    let exp = format!("{x:.5e}");
    let (mant, e) = exp.split_once('e').expect("exponent form");
    let e: i32 = e.parse().expect("exponent");
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
    let mut r = csv::Reader::from_path(path).with_context(|| format!("open {}", path.display()))?;
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
    }

    #[test]
    fn mean_ignores_missing() {
        assert_eq!(mean(&[Some(1.0), None, Some(2.0)]), Some(1.5));
        assert_eq!(mean(&[None]), None);
        assert_eq!(rng(&[Some(3.0), None, Some(-1.0)]), (Some(-1.0), Some(3.0)));
    }
}
