//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical Apache AGE `agtype` value model and text rendering.
//!
//! AGE renders every Cypher result as `agtype` text: JSON-like output
//! with JSONB object-key ordering (shorter keys first, ties bytewise),
//! `::vertex` / `::edge` / `::path` suffixes on graph entities,
//! `PostgreSQL` `float8out` shortest-round-trip float formatting (plus
//! a trailing `.0` when the output would otherwise look integral), and
//! `", "` / `": "` separators. This module reproduces that rendering
//! byte-for-byte against AGE 1.6.0 and defines the total ordering
//! `agtype` uses for `ORDER BY` and comparison operators:
//! path < edge < vertex < object < list < string < bool < number < null.
//!
//! Graph entities travel through the Cypher pipeline as tagged
//! [`Value::Map`] envelopes (see [`AGTYPE_KIND_KEY`]) so vertices,
//! edges, and paths survive `WITH` projections, `collect(...)`, and
//! map/list nesting without a dedicated enum variant in `uqa_core`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use uqa_core::{Edge, Value, Vertex};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgtypeConversionError {
    #[error("graph id {id} cannot be represented as an agtype integer")]
    GraphIdOutOfRange { id: u64 },
}

fn graph_id_value(id: u64) -> Result<Value, AgtypeConversionError> {
    i64::try_from(id)
        .map(Value::Int)
        .map_err(|_| AgtypeConversionError::GraphIdOutOfRange { id })
}

/// Reserved map key that tags a [`Value::Map`] as a graph-entity
/// envelope. The key cannot be produced by Cypher map literals (map
/// keys are identifiers or quoted names without `@` in this dialect).
pub const AGTYPE_KIND_KEY: &str = "@agtype";

const KIND_VERTEX: &str = "vertex";
const KIND_EDGE: &str = "edge";
const KIND_PATH: &str = "path";

/// Entity kind carried by an agtype envelope map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Vertex,
    Edge,
    Path,
}

/// Wrap a [`Vertex`] into its agtype envelope value.
pub fn vertex_to_value(vertex: &Vertex) -> Result<Value, AgtypeConversionError> {
    let mut map = BTreeMap::new();
    map.insert(AGTYPE_KIND_KEY.into(), Value::Str(KIND_VERTEX.into()));
    map.insert("id".into(), graph_id_value(vertex.vertex_id)?);
    map.insert("label".into(), Value::Str(vertex.label.clone()));
    map.insert("properties".into(), Value::Map(vertex.properties.clone()));
    Ok(Value::Map(map))
}

/// Wrap an [`Edge`] into its agtype envelope value.
pub fn edge_to_value(edge: &Edge) -> Result<Value, AgtypeConversionError> {
    let mut map = BTreeMap::new();
    map.insert(AGTYPE_KIND_KEY.into(), Value::Str(KIND_EDGE.into()));
    map.insert("id".into(), graph_id_value(edge.edge_id)?);
    map.insert("label".into(), Value::Str(edge.label.clone()));
    map.insert("start_id".into(), graph_id_value(edge.source_id)?);
    map.insert("end_id".into(), graph_id_value(edge.target_id)?);
    map.insert("properties".into(), Value::Map(edge.properties.clone()));
    Ok(Value::Map(map))
}

/// Wrap an ordered vertex/edge element sequence into a path envelope.
/// Elements must already be vertex / edge envelopes.
pub fn path_to_value(elements: Vec<Value>) -> Value {
    let mut map = BTreeMap::new();
    map.insert(AGTYPE_KIND_KEY.into(), Value::Str(KIND_PATH.into()));
    map.insert("elements".into(), Value::List(elements));
    Value::Map(map)
}

/// Entity kind of an envelope value, or `None` for plain values.
pub fn entity_kind(value: &Value) -> Option<EntityKind> {
    let Value::Map(map) = value else {
        return None;
    };
    match map.get(AGTYPE_KIND_KEY) {
        Some(Value::Str(kind)) if kind == KIND_VERTEX => Some(EntityKind::Vertex),
        Some(Value::Str(kind)) if kind == KIND_EDGE => Some(EntityKind::Edge),
        Some(Value::Str(kind)) if kind == KIND_PATH => Some(EntityKind::Path),
        _ => None,
    }
}

/// Graph id of a vertex / edge envelope.
pub fn entity_id(value: &Value) -> Option<i64> {
    match (entity_kind(value)?, value) {
        (EntityKind::Vertex | EntityKind::Edge, Value::Map(map)) => match map.get("id") {
            Some(Value::Int(id)) => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

/// Label of a vertex / edge envelope.
pub fn entity_label(value: &Value) -> Option<&str> {
    match (entity_kind(value)?, value) {
        (EntityKind::Vertex | EntityKind::Edge, Value::Map(map)) => match map.get("label") {
            Some(Value::Str(label)) => Some(label),
            _ => None,
        },
        _ => None,
    }
}

/// Property map of a vertex / edge envelope.
pub fn entity_properties(value: &Value) -> Option<&BTreeMap<String, Value>> {
    match (entity_kind(value)?, value) {
        (EntityKind::Vertex | EntityKind::Edge, Value::Map(map)) => match map.get("properties") {
            Some(Value::Map(props)) => Some(props),
            _ => None,
        },
        _ => None,
    }
}

/// `start_id` of an edge envelope.
pub fn edge_start_id(value: &Value) -> Option<i64> {
    match (entity_kind(value)?, value) {
        (EntityKind::Edge, Value::Map(map)) => match map.get("start_id") {
            Some(Value::Int(id)) => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

/// `end_id` of an edge envelope.
pub fn edge_end_id(value: &Value) -> Option<i64> {
    match (entity_kind(value)?, value) {
        (EntityKind::Edge, Value::Map(map)) => match map.get("end_id") {
            Some(Value::Int(id)) => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

/// Ordered elements of a path envelope.
pub fn path_elements(value: &Value) -> Option<&[Value]> {
    match (entity_kind(value)?, value) {
        (EntityKind::Path, Value::Map(map)) => match map.get("elements") {
            Some(Value::List(elements)) => Some(elements),
            _ => None,
        },
        _ => None,
    }
}

/// AGE `agtype_value_type` enum ordinal, used verbatim inside AGE
/// error messages such as `abs() unsupported argument agtype 1`.
pub fn agtype_type_ordinal(value: &Value) -> u8 {
    match entity_kind(value) {
        Some(EntityKind::Vertex) => 6,
        Some(EntityKind::Edge) => 7,
        Some(EntityKind::Path) => 8,
        None => match value {
            Value::Null => 0,
            Value::Str(_) | Value::FixedChar(_) => 1,
            Value::Decimal(_) => 2,
            Value::Int(_) => 3,
            Value::Float(_) => 4,
            Value::Bool(_) => 5,
            Value::Array(_) | Value::List(_) | Value::Row(_) => 9,
            Value::Record(_) | Value::Map(_) => 10,
            Value::Json(text) | Value::JsonB(text) => json_type_ordinal(text),
            Value::Bytes(_) | Value::Temporal(_) => 11,
        },
    }
}

/// Human-readable agtype type name used in cast error messages
/// (`cannot cast agtype integer to type boolean`).
pub fn agtype_type_name(value: &Value) -> &'static str {
    match entity_kind(value) {
        Some(EntityKind::Vertex) => "vertex",
        Some(EntityKind::Edge) => "edge",
        Some(EntityKind::Path) => "path",
        None => match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::Decimal(_) => "numeric",
            Value::Str(_) | Value::FixedChar(_) => "string",
            Value::Array(_) | Value::List(_) | Value::Row(_) => "list",
            Value::Record(_) | Value::Map(_) => "map",
            Value::Json(text) | Value::JsonB(text) => json_type_name(text),
            Value::Bytes(_) => "bytea",
            Value::Temporal(_) => "temporal",
        },
    }
}

// ---------------------------------------------------------------------
// Float formatting
// ---------------------------------------------------------------------

/// `PostgreSQL` `float8out` shortest-round-trip formatting: fixed
/// notation while the decimal exponent is in `[-4, 15)`, scientific
/// (`1e+15`, `1e-05`) otherwise, `NaN` / `Infinity` spelled out.
pub fn format_float_pg(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // `{:e}` prints the shortest round-trip mantissa in scientific
    // form (`-3.25e-2`); re-shape it into PostgreSQL conventions.
    let sci = format!("{f:e}");
    let Some((mantissa, exp)) = sci.split_once('e') else {
        return sci;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return sci;
    };
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };

    if (-4..15).contains(&exp) {
        if exp >= 0 {
            let Ok(int_len) = usize::try_from(exp + 1) else {
                return sci;
            };
            if digits.len() > int_len {
                format!("{sign}{}.{}", &digits[..int_len], &digits[int_len..])
            } else {
                let zeros = "0".repeat(int_len - digits.len());
                format!("{sign}{digits}{zeros}")
            }
        } else {
            let Ok(zero_count) = usize::try_from(-exp - 1) else {
                return sci;
            };
            let zeros = "0".repeat(zero_count);
            format!("{sign}0.{zeros}{digits}")
        }
    } else {
        let mantissa_text = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!("{sign}{mantissa_text}e{exp:+03}")
    }
}

/// agtype float rendering: `float8out` plus a trailing `.0` when the
/// output has no `.` / exponent / special marker, so floats stay
/// visually distinct from integers (`100.0`, `-0.0`, `1e+15`, `NaN`).
pub fn format_float_agtype(f: f64) -> String {
    let text = format_float_pg(f);
    if text.bytes().any(|b| matches!(b, b'.' | b'e' | b'N' | b'I')) {
        text
    } else {
        format!("{text}.0")
    }
}

// ---------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------

/// Canonical agtype text of a value. Top-level SQL NULL handling is
/// the caller's concern; a bare `Value::Null` renders as `null` (the
/// in-container spelling).
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    render_into(value, &mut out);
    out
}

fn render_into(value: &Value, out: &mut String) {
    match entity_kind(value) {
        Some(EntityKind::Vertex) => {
            render_entity_body(value, &["id", "label", "properties"], out);
            out.push_str("::vertex");
        }
        Some(EntityKind::Edge) => {
            render_entity_body(
                value,
                &["id", "label", "end_id", "start_id", "properties"],
                out,
            );
            out.push_str("::edge");
        }
        Some(EntityKind::Path) => {
            out.push('[');
            if let Some(elements) = path_elements(value) {
                for (i, element) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render_into(element, out);
                }
            }
            out.push_str("]::path");
        }
        None => match value {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Float(f) => out.push_str(&format_float_agtype(*f)),
            Value::Decimal(d) => {
                out.push_str(&d.to_sql_string());
                out.push_str("::numeric");
            }
            Value::Str(s) => render_json_string(s, out),
            Value::FixedChar(s) => render_json_string(s.trim_end_matches(' '), out),
            Value::Bytes(b) => render_json_string(&String::from_utf8_lossy(b), out),
            Value::Temporal(t) => render_json_string(&t.to_sql_string(), out),
            Value::Json(text) | Value::JsonB(text) => out.push_str(text),
            Value::Array(array) => {
                out.push('[');
                for (index, item) in array.elements().iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    render_into(item, out);
                }
                out.push(']');
            }
            Value::List(items) | Value::Row(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render_into(item, out);
                }
                out.push(']');
            }
            Value::Record(fields) => {
                out.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    render_json_string(name, out);
                    out.push_str(": ");
                    render_into(value, out);
                }
                out.push('}');
            }
            Value::Map(map) => {
                out.push('{');
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_by(|a, b| jsonb_key_cmp(a, b));
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render_json_string(key, out);
                    out.push_str(": ");
                    render_into(&map[*key], out);
                }
                out.push('}');
            }
        },
    }
}

/// Render vertex / edge envelope bodies with AGE's fixed field order
/// (which coincides with JSONB key ordering for these field names).
fn render_entity_body(value: &Value, fields: &[&str], out: &mut String) {
    let Value::Map(map) = value else {
        return;
    };
    out.push('{');
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        render_json_string(field, out);
        out.push_str(": ");
        render_into(map.get(*field).unwrap_or(&Value::Null), out);
    }
    out.push('}');
}

/// JSONB object-key ordering: shorter keys first, ties bytewise.
pub fn jsonb_key_cmp(a: &str, b: &str) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

fn render_json_string(s: &str, out: &mut String) {
    use std::fmt::Write as _;
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------
// Ordering and equality
// ---------------------------------------------------------------------

/// agtype type sort priority (verified against AGE 1.6.0):
/// path < edge < vertex < object < list < string < bool < number < null.
fn sort_priority(value: &Value) -> u8 {
    match entity_kind(value) {
        Some(EntityKind::Path) => 0,
        Some(EntityKind::Edge) => 1,
        Some(EntityKind::Vertex) => 2,
        None => match value {
            Value::Record(_) | Value::Map(_) => 3,
            Value::Array(_) | Value::List(_) | Value::Row(_) => 4,
            Value::Json(text) | Value::JsonB(text) => json_sort_priority(text),
            Value::Str(_) | Value::FixedChar(_) | Value::Bytes(_) | Value::Temporal(_) => 5,
            Value::Bool(_) => 6,
            Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 7,
            Value::Null => 8,
        },
    }
}

fn json_type_ordinal(text: &str) -> u8 {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Null) => 0,
        Ok(serde_json::Value::String(_)) | Err(_) => 1,
        Ok(serde_json::Value::Number(number)) if number.is_i64() || number.is_u64() => 3,
        Ok(serde_json::Value::Number(_)) => 4,
        Ok(serde_json::Value::Bool(_)) => 5,
        Ok(serde_json::Value::Array(_)) => 9,
        Ok(serde_json::Value::Object(_)) => 10,
    }
}

fn json_type_name(text: &str) -> &'static str {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Null) => "null",
        Ok(serde_json::Value::Bool(_)) => "boolean",
        Ok(serde_json::Value::Number(number)) if number.is_i64() || number.is_u64() => "integer",
        Ok(serde_json::Value::Number(_)) => "float",
        Ok(serde_json::Value::String(_)) | Err(_) => "string",
        Ok(serde_json::Value::Array(_)) => "list",
        Ok(serde_json::Value::Object(_)) => "map",
    }
}

fn json_sort_priority(text: &str) -> u8 {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(_)) => 3,
        Ok(serde_json::Value::Array(_)) => 4,
        Ok(serde_json::Value::String(_)) | Err(_) => 5,
        Ok(serde_json::Value::Bool(_)) => 6,
        Ok(serde_json::Value::Number(_)) => 7,
        Ok(serde_json::Value::Null) => 8,
    }
}

/// Total order over agtype values, matching AGE's `ORDER BY`
/// semantics (ascending; `null` sorts last, so `DESC` puts it first).
pub fn cmp(a: &Value, b: &Value) -> Ordering {
    let pa = sort_priority(a);
    let pb = sort_priority(b);
    if pa != pb {
        return pa.cmp(&pb);
    }
    match (entity_kind(a), entity_kind(b)) {
        (Some(EntityKind::Vertex), Some(EntityKind::Vertex))
        | (Some(EntityKind::Edge), Some(EntityKind::Edge)) => entity_id(a).cmp(&entity_id(b)),
        (Some(EntityKind::Path), Some(EntityKind::Path)) => {
            let ea = path_elements(a).unwrap_or(&[]);
            let eb = path_elements(b).unwrap_or(&[]);
            cmp_slices(ea, eb)
        }
        _ => match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            _ if is_number(a) && is_number(b) => cmp_numbers(a, b),
            (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
            (Value::List(x), Value::List(y)) => cmp_slices(x, y),
            (Value::Map(x), Value::Map(y)) => {
                // JSONB object ordering: pair count first, then pairs
                // in key order.
                x.len().cmp(&y.len()).then_with(|| {
                    let mut xs: Vec<(&String, &Value)> = x.iter().collect();
                    let mut ys: Vec<(&String, &Value)> = y.iter().collect();
                    xs.sort_by(|l, r| jsonb_key_cmp(l.0, r.0));
                    ys.sort_by(|l, r| jsonb_key_cmp(l.0, r.0));
                    for ((ka, va), (kb, vb)) in xs.iter().zip(ys.iter()) {
                        let key_cmp = jsonb_key_cmp(ka, kb);
                        if key_cmp != Ordering::Equal {
                            return key_cmp;
                        }
                        let val_cmp = cmp(va, vb);
                        if val_cmp != Ordering::Equal {
                            return val_cmp;
                        }
                    }
                    Ordering::Equal
                })
            }
            (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
            (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
            // Mixed string-like fallbacks within the same priority tier.
            _ => render(a).cmp(&render(b)),
        },
    }
}

fn cmp_slices(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let c = cmp(x, y);
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

fn is_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Float(_) | Value::Decimal(_))
}

fn cmp_numbers(a: &Value, b: &Value) -> Ordering {
    a.cmp(b)
}

/// agtype equality (`=` / `<>` with non-null operands): numbers
/// compare by value across int / float, everything else structurally
/// via the total order.
pub fn eq(a: &Value, b: &Value) -> bool {
    cmp(a, b) == Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(id: u64, label: &str, props: &[(&str, Value)]) -> Vertex {
        let mut v = Vertex::new(id, label);
        for (k, val) in props {
            v.properties.insert((*k).into(), val.clone());
        }
        v
    }

    #[test]
    fn renders_vertex_in_age_format() {
        let v = vertex(
            844_424_930_131_969,
            "Person",
            &[
                ("age", Value::Int(30)),
                ("name", Value::Str("Alice".into())),
            ],
        );
        assert_eq!(
            render(&vertex_to_value(&v).unwrap()),
            "{\"id\": 844424930131969, \"label\": \"Person\", \
             \"properties\": {\"age\": 30, \"name\": \"Alice\"}}::vertex"
        );
    }

    #[test]
    fn renders_edge_in_age_format() {
        let mut e = Edge::new(
            1_125_899_906_842_625,
            844_424_930_131_969,
            844_424_930_131_970,
            "KNOWS",
        );
        e.properties.insert("since".into(), Value::Int(2020));
        assert_eq!(
            render(&edge_to_value(&e).unwrap()),
            "{\"id\": 1125899906842625, \"label\": \"KNOWS\", \
             \"end_id\": 844424930131970, \"start_id\": 844424930131969, \
             \"properties\": {\"since\": 2020}}::edge"
        );
    }

    #[test]
    fn renders_path_with_suffix_at_end() {
        let a = vertex(1, "A", &[]);
        let b = vertex(2, "B", &[]);
        let e = Edge::new(10, 1, 2, "R");
        let path = path_to_value(vec![
            vertex_to_value(&a).unwrap(),
            edge_to_value(&e).unwrap(),
            vertex_to_value(&b).unwrap(),
        ]);
        let text = render(&path);
        assert!(text.starts_with("[{\"id\": 1, \"label\": \"A\""));
        assert!(text.ends_with("::vertex]::path"));
        assert!(text.contains("}::edge, {"));
    }

    #[test]
    fn map_keys_use_jsonb_order() {
        let mut map = BTreeMap::new();
        map.insert("aa".into(), Value::Int(2));
        map.insert("b".into(), Value::Int(1));
        assert_eq!(render(&Value::Map(map)), "{\"b\": 1, \"aa\": 2}");
    }

    #[test]
    fn float_formatting_matches_age() {
        assert_eq!(format_float_agtype(1.0 / 3.0), "0.3333333333333333");
        assert_eq!(
            format_float_agtype(9_223_372_036_854_775_807.0_f64),
            "9.223372036854776e+18"
        );
        assert_eq!(format_float_agtype(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(format_float_agtype(4.0), "4.0");
        assert_eq!(format_float_agtype(100.0), "100.0");
        assert_eq!(format_float_agtype(1_000_000.0), "1000000.0");
        assert_eq!(format_float_agtype(-0.0), "-0.0");
        assert_eq!(format_float_agtype(1e15), "1e+15");
        assert_eq!(format_float_agtype(1e14), "100000000000000.0");
        assert_eq!(format_float_agtype(0.0001), "0.0001");
        assert_eq!(format_float_agtype(0.00001), "1e-05");
        assert_eq!(format_float_agtype(1e100), "1e+100");
        assert_eq!(format_float_agtype(123_456_789.123), "123456789.123");
        assert_eq!(format_float_agtype(f64::NAN), "NaN");
        assert_eq!(format_float_agtype(f64::INFINITY), "Infinity");
        assert_eq!(format_float_agtype(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(format_float_agtype(1.5), "1.5");
        assert_eq!(
            format_float_agtype(std::f64::consts::E),
            "2.718281828459045"
        );
    }

    #[test]
    fn float_pg_formatting_omits_integral_suffix() {
        assert_eq!(format_float_pg(1.0), "1");
        assert_eq!(format_float_pg(1.5), "1.5");
        assert_eq!(format_float_pg(100.0), "100");
    }

    #[test]
    fn scalar_rendering() {
        assert_eq!(render(&Value::Null), "null");
        assert_eq!(render(&Value::Bool(true)), "true");
        assert_eq!(render(&Value::Bool(false)), "false");
        assert_eq!(render(&Value::Int(42)), "42");
        assert_eq!(render(&Value::Str("a\"b\n".into())), "\"a\\\"b\\n\"");
        assert_eq!(
            render(&Value::List(vec![
                Value::Int(1),
                Value::Null,
                Value::Str("x".into()),
            ])),
            "[1, null, \"x\"]"
        );
    }

    #[test]
    fn total_order_matches_age_type_ranks() {
        let vertex_value = vertex_to_value(&vertex(1, "A", &[])).unwrap();
        let edge_value = edge_to_value(&Edge::new(2, 1, 1, "R")).unwrap();
        let path_value = path_to_value(vec![vertex_to_value(&vertex(1, "A", &[])).unwrap()]);
        let mut values = vec![
            Value::Null,
            Value::Float(2.5),
            Value::Int(1),
            Value::Bool(true),
            Value::Str("a".into()),
            Value::List(vec![Value::Int(1)]),
            Value::Map(BTreeMap::from([("x".to_string(), Value::Int(1))])),
            vertex_value.clone(),
            edge_value.clone(),
            path_value.clone(),
        ];
        values.sort_by(cmp);
        let ranks: Vec<u8> = values.iter().map(sort_priority).collect();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4, 5, 6, 7, 7, 8]);
        // Number ties break by value: 1 before 2.5.
        assert_eq!(values[7], Value::Int(1));
        assert_eq!(values[8], Value::Float(2.5));
    }

    #[test]
    fn string_order_is_bytewise_not_length_first() {
        assert_eq!(
            cmp(&Value::Str("ab".into()), &Value::Str("b".into())),
            Ordering::Less
        );
    }

    #[test]
    fn numeric_equality_spans_int_and_float() {
        assert!(eq(&Value::Int(1), &Value::Float(1.0)));
        assert!(!eq(&Value::Int(1), &Value::Str("1".into())));
    }
}
