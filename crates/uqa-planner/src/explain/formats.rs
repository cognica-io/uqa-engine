//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream JSON, XML and YAML from the same borrowed properties into an admitted destination.

use super::properties::{Kind, Property, View};
use uqa_core::memory::{Produced, ProductionControl, ProductionString};
use uqa_sql::{explain::ExplainFormat, SQLError};

struct Writer<'a> {
    output: ProductionString<'a>,
    control: ProductionControl<'a>,
}

pub(super) fn render(
    query: &Property<'_>,
    format: ExplainFormat,
    utility: Option<&str>,
    control: ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut writer = Writer {
        output: ProductionString::new(control),
        control,
    };
    match format {
        ExplainFormat::Json => {
            writer.output.push_str("[\n  ")?;
            writer.json(View::Native(query), 1)?;
            writer.output.push_str("\n]")?;
        }
        ExplainFormat::Xml => {
            writer
                .output
                .push_str("<explain xmlns=\"http://www.postgresql.org/2009/explain\">\n")?;
            if let Some(label) = utility {
                writer.output.push_str("  <")?;
                writer.xml_tag(label)?;
                writer.output.push_str(" />\n")?;
            } else {
                writer.xml("Query", View::Native(query), 1)?;
            }
            writer.output.push_str("</explain>")?;
        }
        ExplainFormat::Yaml => {
            if matches!(View::Native(query).kind(), Kind::Text(_)) {
                writer.output.push_str("- ")?;
                writer.json(View::Native(query), 0)?;
            } else {
                writer.output.push_str("-\n")?;
                writer.yaml(View::Native(query), 1)?;
            }
        }
        ExplainFormat::Text => unreachable!("text plans use their native line renderer"),
    }
    Ok(writer.output.finish()?)
}

pub(super) fn json(
    property: &Property<'_>,
    control: ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut writer = Writer {
        output: ProductionString::new(control),
        control,
    };
    writer.json(View::Native(property), 0)?;
    Ok(writer.output.finish()?)
}

impl Writer<'_> {
    fn indent(&mut self, depth: usize) -> Result<(), SQLError> {
        for _ in 0..depth {
            self.output.push_str("  ")?;
        }
        Ok(())
    }

    fn quoted(&mut self, text: &str) -> Result<(), SQLError> {
        let quoted = uqa_sql::expr::quote_json_string_with_control(text, &self.control)?;
        self.output.push_str(&quoted)?;
        Ok(())
    }

    fn json(&mut self, value: View<'_>, depth: usize) -> Result<(), SQLError> {
        self.control.check()?;
        match value.kind() {
            Kind::Null => self.output.push_str("null")?,
            Kind::Bool(value) => self.output.push_str(if value { "true" } else { "false" })?,
            Kind::Text(text) => self.quoted(text)?,
            Kind::Number(value) => self
                .output
                .push_str(&self.control.format(format_args!("{value}"))?)?,
            Kind::Object(fields) => {
                self.output.push('{')?;
                let mut fields = fields.peekable();
                let empty = fields.peek().is_none();
                while let Some((key, value)) = fields.next() {
                    self.output.push('\n')?;
                    self.indent(depth + 1)?;
                    self.quoted(key)?;
                    self.output.push_str(": ")?;
                    self.json(value, depth + 1)?;
                    if fields.peek().is_some() {
                        self.output.push(',')?;
                    }
                }
                if !empty {
                    self.output.push('\n')?;
                    self.indent(depth)?;
                }
                self.output.push('}')?;
            }
            Kind::Array(values) => {
                self.output.push('[')?;
                let mut values = values.peekable();
                let empty = values.peek().is_none();
                while let Some(value) = values.next() {
                    self.output.push('\n')?;
                    self.indent(depth + 1)?;
                    self.json(value, depth + 1)?;
                    if values.peek().is_some() {
                        self.output.push(',')?;
                    }
                }
                if !empty {
                    self.output.push('\n')?;
                    self.indent(depth)?;
                }
                self.output.push(']')?;
            }
        }
        Ok(())
    }

    fn xml_tag(&mut self, key: &str) -> Result<(), SQLError> {
        if !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            self.output.push_str("Property-")?;
        }
        for c in key.chars() {
            self.output
                .push(if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                })?;
        }
        Ok(())
    }

    fn xml_text(&mut self, text: &str) -> Result<(), SQLError> {
        for c in text.chars() {
            match c {
                '&' => self.output.push_str("&amp;")?,
                '<' => self.output.push_str("&lt;")?,
                '>' => self.output.push_str("&gt;")?,
                '"' => self.output.push_str("&quot;")?,
                '\'' => self.output.push_str("&apos;")?,
                c => self.output.push(c)?,
            }
        }
        Ok(())
    }

    fn xml(&mut self, key: &str, value: View<'_>, depth: usize) -> Result<(), SQLError> {
        self.control.check()?;
        self.indent(depth)?;
        self.output.push('<')?;
        self.xml_tag(key)?;
        self.output.push('>')?;
        match value.kind() {
            Kind::Object(fields) => {
                self.output.push('\n')?;
                for (key, value) in fields {
                    self.xml(key, value, depth + 1)?;
                }
                self.indent(depth)?;
            }
            Kind::Array(values) => {
                self.output.push('\n')?;
                for value in values {
                    self.xml("Item", value, depth + 1)?;
                }
                self.indent(depth)?;
            }
            Kind::Text(text) => self.xml_text(text)?,
            Kind::Null => {}
            Kind::Bool(value) => self.output.push_str(if value { "true" } else { "false" })?,
            Kind::Number(value) => self
                .output
                .push_str(&self.control.format(format_args!("{value}"))?)?,
        }
        self.output.push_str("</")?;
        self.xml_tag(key)?;
        self.output.push_str(">\n")?;
        Ok(())
    }

    fn yaml(&mut self, value: View<'_>, depth: usize) -> Result<(), SQLError> {
        self.control.check()?;
        match value.kind() {
            Kind::Object(fields) => {
                let mut fields = fields.peekable();
                if fields.peek().is_none() {
                    self.indent(depth)?;
                    self.output.push_str("{}\n")?;
                }
                for (key, value) in fields {
                    self.indent(depth)?;
                    self.quoted(key)?;
                    self.output.push(':')?;
                    self.yaml_child(value, depth)?;
                }
            }
            Kind::Array(values) => {
                let mut values = values.peekable();
                if values.peek().is_none() {
                    self.indent(depth)?;
                    self.output.push_str("[]\n")?;
                }
                for value in values {
                    self.indent(depth)?;
                    self.output.push('-')?;
                    self.yaml_child(value, depth)?;
                }
            }
            _ => {
                self.indent(depth)?;
                self.json(value, depth)?;
                self.output.push('\n')?;
            }
        }
        Ok(())
    }

    fn yaml_child(&mut self, value: View<'_>, depth: usize) -> Result<(), SQLError> {
        let nested = match value.kind() {
            Kind::Object(mut fields) => fields.next().is_some(),
            Kind::Array(mut values) => values.next().is_some(),
            _ => false,
        };
        if nested {
            self.output.push('\n')?;
            self.yaml(value, depth + 1)
        } else {
            self.output.push(' ')?;
            self.json(value, depth)?;
            self.output.push('\n')?;
            Ok(())
        }
    }
}
