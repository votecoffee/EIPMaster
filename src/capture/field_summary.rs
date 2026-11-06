use crate::model::{CaptureField, Selection, Value};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct FieldSummary {
    pub name: String,
    pub selection: Selection,
    pub first_value: String,
    pub last_value: String,
    pub minimum: Option<String>,
    pub maximum: Option<String>,
    pub changes_within_generation: u64,
}

#[derive(Default)]
pub(super) struct FieldStats {
    first: Option<String>,
    last: String,
    minimum: Option<Number>,
    maximum: Option<Number>,
    changes: u64,
}

impl FieldStats {
    pub(super) fn record(&mut self, text: &str, value: Value<&[u8]>, same_generation: bool) {
        self.first.get_or_insert_with(|| text.to_owned());
        if self.last != text {
            self.changes += u64::from(same_generation);
            self.last.clear();
            self.last.push_str(text);
        }
        if let Some(number) = Number::finite(value) {
            if self.minimum.is_none_or(|min| number < min) {
                self.minimum = Some(number);
            }
            if self.maximum.is_none_or(|max| number > max) {
                self.maximum = Some(number);
            }
        }
    }

    pub(super) fn finish(self, field: &CaptureField) -> Option<FieldSummary> {
        Some(FieldSummary {
            name: field.name.clone(),
            selection: field.selection,
            first_value: self.first?,
            last_value: self.last,
            minimum: self.minimum.map(Number::text),
            maximum: self.maximum.map(Number::text),
            changes_within_generation: self.changes,
        })
    }
}

// One mapping produces one variant. Keep integers out of floating-point
// comparisons so the full range of the assembly's data type is preserved.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Number {
    Unsigned(u64),
    Signed(i64),
    Float32(f32),
    Float64(f64),
}

impl Number {
    fn finite(value: Value<&[u8]>) -> Option<Self> {
        match value {
            Value::Unsigned(n) => Some(Self::Unsigned(n)),
            Value::Signed(n) => Some(Self::Signed(n)),
            Value::Float32(n) if n.is_finite() => Some(Self::Float32(n)),
            Value::Float64(n) if n.is_finite() => Some(Self::Float64(n)),
            _ => None,
        }
    }

    fn text(self) -> String {
        match self {
            Self::Unsigned(n) => n.to_string(),
            Self::Signed(n) => n.to_string(),
            Self::Float32(n) => n.to_string(),
            Self::Float64(n) => n.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    #[test]
    fn ranges_preserve_full_width_integers_and_ignore_nonfinite_floats() {
        let mut field = CaptureField {
            name: "signal".into(),
            selection: Selection {
                kind: Kind::U64,
                ..Selection::default()
            },
        };
        let mut stats = FieldStats::default();
        for (value, same_generation) in [(u64::MAX, false), (u64::MAX - 1, true), (7, false)] {
            stats.record(&value.to_string(), Value::Unsigned(value), same_generation);
        }
        let integer = stats.finish(&field).unwrap();
        assert_eq!(integer.first_value, u64::MAX.to_string());
        assert_eq!(integer.maximum, Some(u64::MAX.to_string()));
        assert_eq!(integer.minimum.as_deref(), Some("7"));
        assert_eq!(integer.changes_within_generation, 1);

        field.selection.kind = Kind::F64;
        let mut stats = FieldStats::default();
        for (index, value) in [f64::NAN, -1.5, f64::INFINITY, 0.25]
            .into_iter()
            .enumerate()
        {
            stats.record(&value.to_string(), Value::Float64(value), index > 0);
        }
        let float = stats.finish(&field).unwrap();
        assert_eq!(float.first_value, "NaN");
        assert_eq!(float.minimum.as_deref(), Some("-1.5"));
        assert_eq!(float.maximum.as_deref(), Some("0.25"));
    }
}
