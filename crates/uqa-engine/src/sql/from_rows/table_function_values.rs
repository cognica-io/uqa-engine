//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy generate-series, regex, string, and JSON value streams.

use super::{json_table_arg, json_table_value_to_text, prefix_row, ResultRow, SQLError, Value};

pub(in crate::sql) fn generate_series_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if !(2..=3).contains(&evaluated.len()) {
        return Err(SQLError::TypeMismatch(
            "generate_series requires 2-3 args".into(),
        ));
    }
    let start = generate_series_integer(&evaluated[0], "start")?;
    let end = generate_series_integer(&evaluated[1], "stop")?;
    let increment = evaluated
        .get(2)
        .map_or(Ok(1), |value| generate_series_integer(value, "step"))?;
    if increment == 0 {
        return Err(SQLError::TypeMismatch(
            "generate_series step cannot be 0".into(),
        ));
    }
    let mut current = Some(start);
    Ok(Box::new(std::iter::from_fn(move || {
        let value = current?;
        if (increment > 0 && value > end) || (increment < 0 && value < end) {
            current = None;
            return None;
        }
        current = value.checked_add(increment);
        Some(Value::Int(value))
    })))
}

pub(in crate::sql) fn generate_series_integer(value: &Value, label: &str) -> Result<i64, SQLError> {
    match value {
        Value::Int(value) => Ok(*value),
        Value::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value < -(i64::MIN as f64) =>
        {
            Ok(*value as i64)
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "generate_series {label} must be an integer"
        ))),
    }
}

pub(in crate::sql) struct RegexSplitValues {
    regex: regex::Regex,
    source: String,
    piece_start: usize,
    search_start: usize,
    tail_pending: bool,
    done: bool,
}

impl Iterator for RegexSplitValues {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.tail_pending {
            self.done = true;
            return Some(Value::Str(self.source[self.piece_start..].to_string()));
        }
        let Some(found) = self.regex.find_at(&self.source, self.search_start) else {
            self.done = true;
            return Some(Value::Str(self.source[self.piece_start..].to_string()));
        };
        let piece = self.source[self.piece_start..found.start()].to_string();
        self.piece_start = found.end();
        if found.start() == found.end() {
            if found.end() == self.source.len() {
                self.tail_pending = true;
            } else {
                let advance = self.source[found.end()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
                self.search_start = found.end().saturating_add(advance);
            }
        } else {
            self.search_start = found.end();
        }
        Some(Value::Str(piece))
    }
}

pub(in crate::sql) fn regexp_split_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "regexp_split_to_table requires 2 args".into(),
        ));
    }
    let source = match &evaluated[0] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
    };
    let pattern = match &evaluated[1] {
        Value::Str(value) => value,
        _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
    };
    let regex = regex::Regex::new(pattern)
        .map_err(|error| SQLError::TypeMismatch(format!("invalid regex: {error}")))?;
    Ok(Box::new(RegexSplitValues {
        regex,
        source,
        piece_start: 0,
        search_start: 0,
        tail_pending: false,
        done: false,
    }))
}

pub(in crate::sql) fn string_to_table_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "string_to_table requires 2 args".into(),
        ));
    }
    let source = match &evaluated[0] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("string_to_table arg 1".into())),
    };
    let delimiter = match &evaluated[1] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("string_to_table arg 2".into())),
    };
    Ok(Box::new(LiteralSplitValues {
        source,
        delimiter,
        cursor: 0,
        done: false,
    }))
}

pub(in crate::sql) struct LiteralSplitValues {
    source: String,
    delimiter: String,
    cursor: usize,
    done: bool,
}

impl Iterator for LiteralSplitValues {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.delimiter.is_empty() {
            let value = self.source[self.cursor..].chars().next()?;
            self.cursor += value.len_utf8();
            if self.cursor == self.source.len() {
                self.done = true;
            }
            return Some(Value::Str(value.to_string()));
        }
        if let Some(relative) = self.source[self.cursor..].find(&self.delimiter) {
            let delimiter_start = self.cursor + relative;
            let piece = self.source[self.cursor..delimiter_start].to_string();
            self.cursor = delimiter_start + self.delimiter.len();
            return Some(Value::Str(piece));
        }
        self.done = true;
        Some(Value::Str(self.source[self.cursor..].to_string()))
    }
}

pub(in crate::sql) fn json_array_values(
    name: &str,
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    let parsed = json_table_arg(&evaluated[0], name)?;
    let serde_json::Value::Array(items) = parsed else {
        return Err(SQLError::TypeMismatch(format!(
            "{name}: argument is not an array"
        )));
    };
    Ok(Box::new(
        items
            .into_iter()
            .map(|value| json_table_value_to_text(&value)),
    ))
}

pub(in crate::sql) fn json_object_key_values(
    name: &str,
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    let parsed = json_table_arg(&evaluated[0], name)?;
    let serde_json::Value::Object(object) = parsed else {
        return Err(SQLError::TypeMismatch(format!(
            "{name}: argument is not an object"
        )));
    };
    Ok(Box::new(object.into_iter().map(|(key, _)| Value::Str(key))))
}

pub(in crate::sql) fn json_each_row_stream(
    name: &str,
    evaluated: Vec<Value>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<uqa_execution::ProjectRows, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    let parsed = json_table_arg(&evaluated[0], name)?;
    let serde_json::Value::Object(object) = parsed else {
        return Err(SQLError::TypeMismatch(format!(
            "{name}: argument is not an object"
        )));
    };
    let key_column = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| "key".into());
    let value_column = column_aliases
        .get(1)
        .cloned()
        .unwrap_or_else(|| "value".into());
    let qualifier = alias.map(str::to_string);
    Ok(Box::new(object.into_iter().map(move |(key, value)| {
        let mut row = ResultRow::new();
        row.insert(key_column.clone(), Value::Str(key));
        row.insert(value_column.clone(), json_table_value_to_text(&value));
        Ok(qualifier
            .as_deref()
            .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row)))
    })))
}
