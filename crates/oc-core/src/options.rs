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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptionSchema {
    pub specs: Vec<OptionSpec>,
}

/// Validated option values, with defaults filled in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options(BTreeMap<&'static str, OptionValue>);

impl Options {
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
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.0.get(key) {
            Some(OptionValue::Text(v)) => Some(v),
            _ => None,
        }
    }
}

impl OptionSchema {
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
