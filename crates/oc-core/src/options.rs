//! Typed writer-option schemas (MASTER_PROMPT §9.4).
use crate::error::{Error, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum OptionKind {
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Float {
        min: f64,
        max: f64,
        default: f64,
    },
    Bool {
        default: bool,
    },
    Enum {
        choices: Vec<&'static str>,
        default: &'static str,
    },
    Color {
        default: [u8; 4],
    },
    Text {
        default: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// Key for the translation catalogue.
    pub i18n_key: &'static str,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
    Color([u8; 4]),
}

impl OptionValue {
    /// Text form accepted by [`OptionSchema::parse_arg`].
    pub fn to_arg(&self) -> String {
        match self {
            Self::Int(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Text(v) => v.clone(),
            Self::Color([r, g, b, a]) => format!("#{r:02x}{g:02x}{b:02x}{a:02x}"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptionSchema {
    pub specs: Vec<OptionSpec>,
}

/// Validated option values, with defaults filled in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options(BTreeMap<&'static str, OptionValue>);

impl Options {
    /// Sets a value the schema declares, for engines that fill in a missing option.
    pub fn set(&mut self, key: &'static str, value: OptionValue) {
        self.0.insert(key, value);
    }

    pub fn get(&self, key: &str) -> Option<&OptionValue> {
        self.0.get(key)
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        match self.0.get(key) {
            Some(OptionValue::Int(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.0.get(key) {
            Some(OptionValue::Bool(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn float(&self, key: &str) -> Option<f64> {
        match self.0.get(key) {
            Some(OptionValue::Float(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn color(&self, key: &str) -> Option<[u8; 4]> {
        match self.0.get(key) {
            Some(OptionValue::Color(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.0.get(key) {
            Some(OptionValue::Text(v)) => Some(v),
            _ => None,
        }
    }
}

impl OptionSchema {
    /// Parse a command-line `key=value` string according to the option's declared type.
    pub fn parse_arg(&self, key: &str, raw: &str) -> Result<OptionValue> {
        let spec = self
            .specs
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| Error::invalid_option(key, "unknown option"))?;
        let bad = || Error::invalid_option(key, format!("cannot read '{raw}' as a value"));
        Ok(match &spec.kind {
            OptionKind::Int { .. } => OptionValue::Int(raw.parse().map_err(|_| bad())?),
            OptionKind::Float { .. } => OptionValue::Float(raw.parse().map_err(|_| bad())?),
            OptionKind::Bool { .. } => match raw {
                "true" | "yes" | "1" => OptionValue::Bool(true),
                "false" | "no" | "0" => OptionValue::Bool(false),
                _ => return Err(bad()),
            },
            OptionKind::Enum { .. } | OptionKind::Text { .. } => OptionValue::Text(raw.to_string()),
            OptionKind::Color { .. } => {
                let h = raw.strip_prefix('#').ok_or_else(bad)?;
                let byte = |i: usize| {
                    h.get(i..i + 2)
                        .and_then(|x| u8::from_str_radix(x, 16).ok())
                        .ok_or_else(bad)
                };
                match h.len() {
                    6 => OptionValue::Color([byte(0)?, byte(2)?, byte(4)?, 255]),
                    8 => OptionValue::Color([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
                    _ => return Err(bad()),
                }
            }
        })
    }

    /// Validate user-supplied raw values against the schema. Unknown keys are rejected.
    pub fn validate(&self, given: &[(String, OptionValue)]) -> Result<Options> {
        for (k, _) in given {
            if !self.specs.iter().any(|s| s.key == k) {
                return Err(Error::invalid_option(k, "unknown option"));
            }
        }
        let mut out = BTreeMap::new();
        for spec in &self.specs {
            let supplied = given
                .iter()
                .rev()
                .find(|(k, _)| k == spec.key)
                .map(|(_, v)| v);
            let value = match (&spec.kind, supplied) {
                (OptionKind::Int { default, .. }, None) => OptionValue::Int(*default),
                (OptionKind::Float { default, .. }, None) => OptionValue::Float(*default),
                (OptionKind::Bool { default }, None) => OptionValue::Bool(*default),
                (OptionKind::Enum { default, .. }, None) => {
                    OptionValue::Text((*default).to_string())
                }
                (OptionKind::Color { default }, None) => OptionValue::Color(*default),
                (OptionKind::Text { default }, None) => OptionValue::Text(default.clone()),
                (OptionKind::Int { min, max, .. }, Some(OptionValue::Int(v))) => {
                    if v < min || v > max {
                        return Err(Error::invalid_option(
                            spec.key,
                            format!("must be {min}..={max}"),
                        ));
                    }
                    OptionValue::Int(*v)
                }
                (OptionKind::Float { min, max, .. }, Some(OptionValue::Float(v))) => {
                    if !(*min..=*max).contains(v) {
                        return Err(Error::invalid_option(
                            spec.key,
                            format!("must be {min}..={max}"),
                        ));
                    }
                    OptionValue::Float(*v)
                }
                (OptionKind::Bool { .. }, Some(OptionValue::Bool(v))) => OptionValue::Bool(*v),
                (OptionKind::Enum { choices, .. }, Some(OptionValue::Text(v))) => {
                    if !choices.contains(&v.as_str()) {
                        return Err(Error::invalid_option(
                            spec.key,
                            format!("must be one of {choices:?}"),
                        ));
                    }
                    OptionValue::Text(v.clone())
                }
                (OptionKind::Color { .. }, Some(OptionValue::Color(v))) => OptionValue::Color(*v),
                (OptionKind::Text { .. }, Some(OptionValue::Text(v))) => {
                    OptionValue::Text(v.clone())
                }
                (_, Some(_)) => return Err(Error::invalid_option(spec.key, "wrong value type")),
            };
            out.insert(spec.key, value);
        }
        Ok(Options(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> OptionSchema {
        OptionSchema {
            specs: vec![
                OptionSpec {
                    key: "quality",
                    label: "Quality",
                    help: "JPEG quality",
                    i18n_key: "opt.quality",
                    kind: OptionKind::Int {
                        min: 1,
                        max: 100,
                        default: 85,
                    },
                },
                OptionSpec {
                    key: "mode",
                    label: "Mode",
                    help: "",
                    i18n_key: "opt.mode",
                    kind: OptionKind::Enum {
                        choices: vec!["fast", "best"],
                        default: "fast",
                    },
                },
            ],
        }
    }

    #[test]
    fn defaults_are_filled() {
        let o = schema().validate(&[]).ok();
        assert_eq!(o.as_ref().and_then(|o| o.int("quality")), Some(85));
        assert_eq!(o.as_ref().and_then(|o| o.text("mode")), Some("fast"));
    }

    #[test]
    fn cli_arguments_follow_declared_types() {
        let s = schema();
        assert_eq!(s.parse_arg("quality", "7").ok(), Some(OptionValue::Int(7)));
        assert!(s.parse_arg("quality", "high").is_err());
        assert_eq!(
            s.parse_arg("mode", "best").ok(),
            Some(OptionValue::Text("best".into()))
        );
        assert!(s.parse_arg("nope", "1").is_err());
        let v = OptionValue::Color([1, 2, 255, 128]);
        let c = OptionSchema {
            specs: vec![OptionSpec {
                key: "bg",
                label: "",
                help: "",
                i18n_key: "",
                kind: OptionKind::Color {
                    default: [0, 0, 0, 255],
                },
            }],
        };
        assert_eq!(c.parse_arg("bg", &v.to_arg()).ok(), Some(v));
    }

    #[test]
    fn range_enum_and_unknown_are_rejected() {
        let s = schema();
        assert!(s
            .validate(&[("quality".into(), OptionValue::Int(0))])
            .is_err());
        assert!(s
            .validate(&[("quality".into(), OptionValue::Int(101))])
            .is_err());
        assert!(s
            .validate(&[("mode".into(), OptionValue::Text("x".into()))])
            .is_err());
        assert!(s.validate(&[("nope".into(), OptionValue::Int(1))]).is_err());
        assert!(s
            .validate(&[("quality".into(), OptionValue::Bool(true))])
            .is_err());
        assert!(s
            .validate(&[("quality".into(), OptionValue::Int(50))])
            .is_ok());
    }
}
