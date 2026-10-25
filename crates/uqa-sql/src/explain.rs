//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered EXPLAIN syntax, result descriptions and execution option interpretation.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::{ColumnType, RowSchema, SQLError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExplainOptionValue {
    Boolean(bool),
    Integer(i32),
    Float(String),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainOption {
    pub name: String,
    pub value: Option<ExplainOptionValue>,
}

/// Retain every occurrence in parser order. Describing a result reads FORMAT; executing the command interprets all options before planning its body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredOptions")]
pub struct ExplainOptions {
    pub options: Vec<ExplainOption>,
}

/// Older stored statements used these three fields. Deserialize them into the same ordered representation; new serialization has only the options list.
#[derive(Deserialize)]
struct StoredOptions {
    options: Option<Vec<ExplainOption>>,
    analyze: Option<bool>,
    verbose: Option<bool>,
    format: Option<String>,
}

impl From<StoredOptions> for ExplainOptions {
    fn from(stored: StoredOptions) -> Self {
        let options = stored.options.unwrap_or_else(|| {
            let mut options = Vec::new();
            for (name, value) in [("analyze", stored.analyze), ("verbose", stored.verbose)] {
                if let Some(value) = value {
                    options.push(ExplainOption {
                        name: name.into(),
                        value: Some(ExplainOptionValue::Boolean(value)),
                    });
                }
            }
            if let Some(format) = stored.format {
                options.push(ExplainOption {
                    name: "format".into(),
                    value: Some(ExplainOptionValue::String(format)),
                });
            }
            options
        });
        Self { options }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExplainFormat {
    #[default]
    Text,
    Xml,
    Json,
    Yaml,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExplainSerialization {
    #[default]
    None,
    Text,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "EXPLAIN options are independent SQL switches"
)]
pub struct ExplainSettings {
    pub analyze: bool,
    pub verbose: bool,
    pub costs: bool,
    pub buffers: bool,
    pub wal: bool,
    pub settings: bool,
    pub generic_plan: bool,
    pub timing: bool,
    pub summary: bool,
    pub memory: bool,
    pub serialize: ExplainSerialization,
    pub format: ExplainFormat,
}

impl Default for ExplainSettings {
    fn default() -> Self {
        Self {
            analyze: false,
            verbose: false,
            costs: true,
            buffers: false,
            wal: false,
            settings: false,
            generic_plan: false,
            timing: false,
            summary: false,
            memory: false,
            serialize: ExplainSerialization::None,
            format: ExplainFormat::Text,
        }
    }
}

impl ExplainOptions {
    /// Ordinary analysis without an external parameter hook reads only `GENERIC_PLAN` before the body. All other option diagnostics retain their execution order.
    pub fn infers_unbound_parameters(&self) -> Result<bool, SQLError> {
        let mut generic = false;
        for option in &self.options {
            if option.name == "generic_plan" {
                generic = option.boolean()?;
            }
        }
        Ok(generic)
    }

    pub fn result_schema(&self) -> Result<RowSchema, SQLError> {
        let mut ty = ColumnType::Text;
        for option in &self.options {
            if option.name == "format" {
                ty = match option.text()?.as_ref() {
                    "xml" => ColumnType::Xml,
                    "json" => ColumnType::Json,
                    _ => ColumnType::Text,
                };
            }
        }
        Ok(RowSchema::with_types(
            vec!["QUERY PLAN".into()],
            vec![Some(ty)],
        ))
    }

    pub fn interpret(&self) -> Result<ExplainSettings, SQLError> {
        let mut settings = ExplainSettings::default();
        let mut timing = None;
        let mut buffers = None;
        let mut summary = None;
        for option in &self.options {
            match option.name.as_str() {
                "analyze" => settings.analyze = option.boolean()?,
                "verbose" => settings.verbose = option.boolean()?,
                "costs" => settings.costs = option.boolean()?,
                "buffers" => buffers = Some(option.boolean()?),
                "wal" => settings.wal = option.boolean()?,
                "settings" => settings.settings = option.boolean()?,
                "generic_plan" => settings.generic_plan = option.boolean()?,
                "timing" => timing = Some(option.boolean()?),
                "summary" => summary = Some(option.boolean()?),
                "memory" => settings.memory = option.boolean()?,
                "serialize" => {
                    settings.serialize = if option.value.is_none() {
                        ExplainSerialization::Text
                    } else {
                        match option.text()?.as_ref() {
                            "off" | "none" => ExplainSerialization::None,
                            "text" => ExplainSerialization::Text,
                            "binary" => ExplainSerialization::Binary,
                            value => return Err(option.unrecognized_value(value)),
                        }
                    };
                }
                "format" => {
                    settings.format = match option.text()?.as_ref() {
                        "text" => ExplainFormat::Text,
                        "xml" => ExplainFormat::Xml,
                        "json" => ExplainFormat::Json,
                        "yaml" => ExplainFormat::Yaml,
                        value => return Err(option.unrecognized_value(value)),
                    };
                }
                name => {
                    return Err(diagnostic(
                        "42601",
                        format!("unrecognized EXPLAIN option \"{name}\""),
                    ));
                }
            }
        }
        settings.timing = timing.unwrap_or(settings.analyze);
        settings.buffers = buffers.unwrap_or(settings.analyze);
        settings.summary = summary.unwrap_or(settings.analyze);
        for (enabled, name) in [
            (settings.wal, "WAL"),
            (settings.timing, "TIMING"),
            (
                settings.serialize != ExplainSerialization::None,
                "SERIALIZE",
            ),
        ] {
            if enabled && !settings.analyze {
                return Err(diagnostic(
                    "22023",
                    format!("EXPLAIN option {name} requires ANALYZE"),
                ));
            }
        }
        if settings.generic_plan && settings.analyze {
            return Err(diagnostic(
                "22023",
                "EXPLAIN options ANALYZE and GENERIC_PLAN cannot be used together".into(),
            ));
        }
        Ok(settings)
    }
}

impl ExplainOption {
    fn text(&self) -> Result<Cow<'_, str>, SQLError> {
        Ok(match &self.value {
            Some(ExplainOptionValue::String(value) | ExplainOptionValue::Float(value)) => {
                value.as_str().into()
            }
            Some(ExplainOptionValue::Integer(value)) => value.to_string().into(),
            Some(ExplainOptionValue::Boolean(value)) => {
                if *value { "true" } else { "false" }.into()
            }
            None => {
                return Err(diagnostic(
                    "42601",
                    format!("{} requires a parameter", self.name),
                ))
            }
        })
    }

    fn boolean(&self) -> Result<bool, SQLError> {
        match &self.value {
            None => return Ok(true),
            Some(ExplainOptionValue::Boolean(value)) => return Ok(*value),
            Some(ExplainOptionValue::Integer(0)) => return Ok(false),
            Some(ExplainOptionValue::Integer(1)) => return Ok(true),
            Some(ExplainOptionValue::String(value)) => {
                if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("on") {
                    return Ok(true);
                }
                if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off") {
                    return Ok(false);
                }
            }
            _ => {}
        }
        Err(diagnostic(
            "42601",
            format!("{} requires a Boolean value", self.name),
        ))
    }

    fn unrecognized_value(&self, value: &str) -> SQLError {
        diagnostic(
            "22023",
            format!(
                "unrecognized value for EXPLAIN option \"{}\": \"{value}\"",
                self.name
            ),
        )
    }
}

fn diagnostic(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

#[cfg(test)]
mod tests;
