//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! XML and YAML encodings of the same native structured explanation.

use serde_json::Value;
use std::fmt::Write as _;
use uqa_sql::SQLError;

pub(super) fn xml_utility(label: &str) -> String {
    format!(
        "<explain xmlns=\"http://www.postgresql.org/2009/explain\">\n  <{} />\n</explain>",
        label.replace(' ', "-")
    )
}

pub(super) fn xml(query: &Value) -> Result<String, SQLError> {
    let mut output = String::from("<explain xmlns=\"http://www.postgresql.org/2009/explain\">\n");
    xml_value(&mut output, "Query", query, 1)?;
    output.push_str("</explain>");
    Ok(output)
}

fn xml_value(output: &mut String, key: &str, value: &Value, depth: usize) -> Result<(), SQLError> {
    let mut tag = String::new();
    if !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        tag.push_str("Property-");
    }
    tag.extend(key.chars().map(|c| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        }
    }));
    let indent = "  ".repeat(depth);
    write!(output, "{indent}<{tag}>").map_err(format_error)?;
    match value {
        Value::Object(fields) => {
            output.push('\n');
            for (key, value) in fields {
                xml_value(output, key, value, depth + 1)?;
            }
            output.push_str(&indent);
        }
        Value::Array(items) => {
            output.push('\n');
            for item in items {
                xml_value(output, "Item", item, depth + 1)?;
            }
            output.push_str(&indent);
        }
        Value::Null => {}
        Value::String(text) => escape_xml(output, text),
        value => escape_xml(output, &value.to_string()),
    }
    writeln!(output, "</{tag}>").map_err(format_error)
}

fn escape_xml(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character => output.push(character),
        }
    }
}

pub(super) fn yaml(query: &Value) -> Result<String, SQLError> {
    if let Value::String(label) = query {
        return Ok(format!("- {}", quoted(label)?));
    }
    let mut output = String::from("-\n");
    yaml_value(&mut output, query, 1)?;
    Ok(output)
}

fn yaml_value(output: &mut String, value: &Value, depth: usize) -> Result<(), SQLError> {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (key, value) in fields {
                write!(output, "{indent}{}:", quoted(key)?).map_err(format_error)?;
                yaml_child(output, value, depth)?;
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                write!(output, "{indent}-").map_err(format_error)?;
                yaml_child(output, item, depth)?;
            }
        }
        value => writeln!(output, "{indent}{value}").map_err(format_error)?,
    }
    Ok(())
}

fn yaml_child(output: &mut String, value: &Value, depth: usize) -> Result<(), SQLError> {
    if matches!(value, Value::Object(fields) if !fields.is_empty())
        || matches!(value, Value::Array(items) if !items.is_empty())
    {
        output.push('\n');
        yaml_value(output, value, depth + 1)
    } else {
        writeln!(output, " {value}").map_err(format_error)
    }
}

fn quoted(text: &str) -> Result<String, SQLError> {
    serde_json::to_string(text).map_err(|error| SQLError::Internal(error.to_string()))
}

fn format_error(error: std::fmt::Error) -> SQLError {
    SQLError::Internal(format!("format native EXPLAIN: {error}"))
}
