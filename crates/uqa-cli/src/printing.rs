//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{SQLResult, Value};
use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;
use uqa_sql::{expr::EngineHook, SQLError};

use crate::variables::parse_boolean;

/// Presentation settings never change Engine values or transaction boundaries.
#[derive(Clone)]
pub(crate) struct PrintOptions {
    pub(crate) format: String,
    pub(crate) expanded: String,
    pub(crate) tuples_only: bool,
    pub(crate) footer: bool,
    pub(crate) pager: String,
    pub(crate) field_separator: String,
    pub(crate) record_separator: String,
    pub(crate) null: String,
    pub(crate) title: String,
    pub(crate) border: usize,
    pub(crate) columns: usize,
    pub(crate) csv_separator: char,
}
impl Default for PrintOptions {
    fn default() -> Self {
        Self {
            format: "aligned".into(),
            expanded: "off".into(),
            tuples_only: false,
            footer: true,
            pager: "on".into(),
            field_separator: "|".into(),
            record_separator: "\n".into(),
            null: String::new(),
            title: String::new(),
            border: 1,
            columns: 80,
            csv_separator: ',',
        }
    }
}
impl PrintOptions {
    pub(super) fn value(&self, key: &str) -> Result<String, String> {
        Ok(match key {
            "format" => self.format.clone(),
            "expanded" => self.expanded.clone(),
            "tuples_only" => self.tuples_only.to_string(),
            "footer" => self.footer.to_string(),
            "pager" => self.pager.clone(),
            "fieldsep" => self.field_separator.clone(),
            "recordsep" => self.record_separator.clone(),
            "null" => self.null.clone(),
            "title" => self.title.clone(),
            "border" => self.border.to_string(),
            "columns" => self.columns.to_string(),
            "csv_fieldsep" => self.csv_separator.to_string(),
            _ => return Err(format!("unknown printing option: {key}")),
        })
    }

    pub(crate) fn set(&mut self, option: &str, value: &str) -> Result<(), String> {
        match option {
            "format" if ["aligned", "unaligned", "csv", "html", "json"].contains(&value) => {
                self.format = value.into();
            }
            "expanded" => {
                self.expanded = if value == "auto" {
                    "auto".into()
                } else if value.is_empty() {
                    if self.expanded == "off" {
                        "on".into()
                    } else {
                        "off".into()
                    }
                } else if parse_boolean(value)? {
                    "on".into()
                } else {
                    "off".into()
                }
            }
            "tuples_only" => {
                self.tuples_only = if value.is_empty() {
                    !self.tuples_only
                } else {
                    parse_boolean(value)?
                }
            }
            "footer" => {
                self.footer = if value.is_empty() {
                    !self.footer
                } else {
                    parse_boolean(value)?
                }
            }
            "pager" => {
                self.pager = if value == "always" {
                    "always".into()
                } else if value.is_empty() {
                    if self.pager == "off" {
                        "on".into()
                    } else {
                        "off".into()
                    }
                } else if parse_boolean(value)? {
                    "on".into()
                } else {
                    "off".into()
                }
            }
            "fieldsep" => self.field_separator = value.into(),
            "recordsep" => self.record_separator = value.into(),
            "fieldsep_zero" => self.field_separator = "\0".into(),
            "recordsep_zero" => self.record_separator = "\0".into(),
            "null" => self.null = value.into(),
            "title" => self.title = value.into(),
            "border" => {
                self.border = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n <= 2)
                    .ok_or("invalid printing option".to_owned())?;
            }
            "columns" => {
                self.columns = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n <= 65_536)
                    .ok_or("invalid printing option".to_owned())?;
            }
            "csv_fieldsep" => {
                let mut chars = value.chars();
                let delimiter = chars
                    .next()
                    .filter(|c| !matches!(c, '"' | '\n' | '\r' | '\0'))
                    .ok_or("invalid printing option".to_owned())?;
                if chars.next().is_some() {
                    return Err("invalid printing option".to_owned());
                }
                self.csv_separator = delimiter;
            }
            _ => return Err("invalid printing option".to_owned()),
        }
        Ok(())
    }
    pub(crate) fn render(&self, response: &DisplayTable, expanded_once: bool) -> String {
        self.render_output(response, expanded_once, false)
    }

    pub(crate) fn render_terminal(&self, response: &DisplayTable, expanded_once: bool) -> String {
        self.render_output(response, expanded_once, true)
    }

    fn render_output(
        &self,
        response: &DisplayTable,
        expanded_once: bool,
        terminal: bool,
    ) -> String {
        if self.format == "json" {
            return serde_json::json!({"columns":response.columns,"rows":response.rows,"affected_rows":response.affected_rows}).to_string() + "\n";
        }
        if !response.has_rows {
            if let Some(tag) = &response.command_tag {
                return format!("{tag}\n");
            }
            return if response.affected_rows > 0 {
                format!("{} row(s) affected\n", response.affected_rows)
            } else {
                String::new()
            };
        }
        if self.format == "csv" {
            return self.render_csv(response, terminal);
        }
        let headers: Vec<_> = response.columns.iter().map(|s| terminal_text(s)).collect();
        let rows: Vec<Vec<String>> = response
            .rows
            .iter()
            .enumerate()
            .map(|(row, _)| {
                response
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(column, _)| {
                        if terminal || self.format == "aligned" {
                            terminal_text(response.value_at(row, column).unwrap_or(&self.null))
                        } else {
                            response
                                .value_at(row, column)
                                .unwrap_or(&self.null)
                                .to_owned()
                        }
                    })
                    .collect()
            })
            .collect();
        match self.format.as_str() {
            "html" => self.render_html(&headers, &rows),
            "unaligned" => self.render_unaligned(&headers, &rows),
            _ => self.render_aligned(response, &headers, &rows, expanded_once),
        }
    }

    fn render_csv(&self, response: &DisplayTable, terminal: bool) -> String {
        let mut result = String::new();
        if !self.tuples_only {
            result.push_str(
                &response
                    .columns
                    .iter()
                    .map(|h| {
                        let text = if terminal {
                            csv_terminal_text(h)
                        } else {
                            h.clone()
                        };
                        csv_cell(&text, self.csv_separator, false)
                    })
                    .collect::<Vec<_>>()
                    .join(&self.csv_separator.to_string()),
            );
            result.push('\n');
        }
        for row in 0..response.rows.len() {
            result.push_str(
                &response
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(column, _)| {
                        let value = response.value_at(row, column);
                        let mut text = value.unwrap_or(&self.null).to_owned();
                        if terminal {
                            text = csv_terminal_text(&text);
                        }
                        csv_cell(&text, self.csv_separator, false)
                    })
                    .collect::<Vec<_>>()
                    .join(&self.csv_separator.to_string()),
            );
            result.push('\n');
        }
        result
    }

    fn render_html(&self, headers: &[String], rows: &[Vec<String>]) -> String {
        let mut output = String::from("<table>\n");
        if !self.title.is_empty() {
            writeln!(output, "<caption>{}</caption>", html(&self.title)).unwrap();
        }
        if !self.tuples_only {
            output.push_str("<tr>");
            for header in headers {
                write!(output, "<th>{}</th>", html(header)).unwrap();
            }
            output.push_str("</tr>\n");
        }
        for row in rows {
            output.push_str("<tr>");
            for value in row {
                write!(output, "<td>{}</td>", html(value)).unwrap();
            }
            output.push_str("</tr>\n");
        }
        output.push_str("</table>\n");
        output
    }

    fn render_unaligned(&self, headers: &[String], rows: &[Vec<String>]) -> String {
        let mut records = Vec::new();
        if !self.tuples_only {
            records.push(headers.join(&self.field_separator));
        }
        records.extend(rows.iter().map(|row| row.join(&self.field_separator)));
        let mut result = records.join(&self.record_separator);
        result.push_str(&self.record_separator);
        result
    }

    fn render_aligned(
        &self,
        response: &DisplayTable,
        headers: &[String],
        rows: &[Vec<String>],
        expanded_once: bool,
    ) -> String {
        let widths: Vec<_> = (0..headers.len())
            .map(|index| {
                rows.iter()
                    .map(|row| UnicodeWidthStr::width(row[index].as_str()))
                    .chain(std::iter::once(UnicodeWidthStr::width(
                        headers[index].as_str(),
                    )))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let width = widths.iter().sum::<usize>() + widths.len().saturating_sub(1) * 3;
        let expanded = expanded_once
            || self.expanded == "on"
            || (self.expanded == "auto" && self.columns > 0 && width > self.columns);
        let mut result = String::new();
        if !self.title.is_empty() {
            writeln!(result, "{}", terminal_text(&self.title)).unwrap();
        }
        if expanded {
            let key_width = headers
                .iter()
                .map(|h| UnicodeWidthStr::width(h.as_str()))
                .max()
                .unwrap_or(0);
            for (index, row) in rows.iter().enumerate() {
                if !self.tuples_only {
                    writeln!(result, "-[ RECORD {} ]-", index + 1).unwrap();
                }
                for (header, value) in headers.iter().zip(row) {
                    writeln!(result, "{} | {value}", padded(header, key_width, false)).unwrap();
                }
            }
        } else {
            let separator = widths
                .iter()
                .map(|width| "-".repeat(*width + if self.border == 0 { 0 } else { 2 }))
                .collect::<Vec<_>>()
                .join(if self.border == 0 { " " } else { "+" });
            let separator = if self.border == 2 {
                format!("+{separator}+\n")
            } else {
                separator + "\n"
            };
            if self.border == 2 {
                result.push_str(&separator);
            }
            if !self.tuples_only {
                result.push_str(&aligned_row(headers, &widths, self.border, None));
                result.push_str(&separator);
            }
            for row in rows {
                result.push_str(&aligned_row(
                    row,
                    &widths,
                    self.border,
                    Some(&response.numeric),
                ));
            }
            if self.border == 2 {
                result.push_str(&separator);
            }
        }
        if self.footer && !self.tuples_only {
            writeln!(
                result,
                "({} {})",
                rows.len(),
                if rows.len() == 1 { "row" } else { "rows" }
            )
            .unwrap();
        }
        result
    }
}
pub(crate) fn terminal_text(source: &str) -> String {
    escape_controls(source, |_| false)
}

fn escape_controls(source: &str, preserve: impl Fn(char) -> bool) -> String {
    let mut output = String::with_capacity(source.len());
    for character in source.chars() {
        if character.is_control() && !preserve(character) {
            match character {
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                _ => write!(output, "\\u{{{:x}}}", u32::from(character))
                    .expect("String writes do not fail"),
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn csv_terminal_text(source: &str) -> String {
    escape_controls(source, |character| matches!(character, '\n' | '\r' | '\t'))
}
fn padded(value: &str, width: usize, right: bool) -> String {
    let padding = " ".repeat(width.saturating_sub(UnicodeWidthStr::width(value)));
    if right {
        padding + value
    } else {
        value.to_owned() + &padding
    }
}
fn aligned_row(
    values: &[String],
    widths: &[usize],
    border: usize,
    numeric: Option<&[bool]>,
) -> String {
    let cells = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            padded(
                v,
                widths[i],
                numeric.is_some_and(|n| n.get(i) == Some(&true)),
            )
        })
        .collect::<Vec<_>>()
        .join(if border == 0 { " " } else { " | " });
    match border {
        2 => format!("| {cells} |\n"),
        1 => format!(" {cells} \n"),
        _ => cells + "\n",
    }
}
fn csv_cell(value: &str, delimiter: char, force: bool) -> String {
    if force || value.contains([delimiter, '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.into()
    }
}
fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Values are formatted by the SQL owner before applying a presentation format.
pub(crate) struct DisplayTable {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    numeric: Vec<bool>,
    affected_rows: u64,
    command_tag: Option<String>,
    has_rows: bool,
}
impl DisplayTable {
    fn value_at(&self, row: usize, column: usize) -> Option<&str> {
        self.rows.get(row)?.get(column)?.as_deref()
    }
    pub(crate) fn from_result(
        result: &SQLResult,
        engine: Option<&dyn EngineHook>,
    ) -> Result<Self, SQLError> {
        let columns = if result.columns.is_empty() {
            result
                .rows
                .iter()
                .flat_map(|row| row.keys().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            result.columns.clone()
        };
        let rows = (0..result.rows.len())
            .map(|row| {
                (0..columns.len())
                    .map(|column| {
                        let value = result.value_at(row, column);
                        if value.is_none_or(|value| matches!(value, Value::Null)) {
                            Ok(None)
                        } else {
                            super::output::value_to_display_typed(
                                value,
                                result.column_types.get(column).and_then(Option::as_ref),
                                engine,
                            )
                            .map(Some)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let numeric = (0..columns.len())
            .map(|column| {
                (0..result.rows.len()).any(|row| {
                    matches!(
                        result.value_at(row, column),
                        Some(Value::Int(_) | Value::Float(_))
                    )
                })
            })
            .collect();
        Ok(Self {
            columns,
            rows,
            numeric,
            affected_rows: result.affected_rows,
            command_tag: result.command_tag.clone(),
            has_rows: result.kind == uqa_sql::SQLResultKind::Rows
                || !result.columns.is_empty()
                || !result.rows.is_empty(),
        })
    }
}

/// Keep message line breaks while making terminal control sequences inert.
pub(super) fn message_text(source: &str) -> String {
    escape_controls(source, |character| matches!(character, '\n' | '\t'))
}
