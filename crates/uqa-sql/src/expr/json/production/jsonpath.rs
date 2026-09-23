//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The existing selector and predicate grammar operates on borrowed, admitted nodes.

use super::{field_index, inline, input, parsed, writer, Node};
use crate::error::{Result, SQLError};
use crate::expr::conversion::value_to_string_with_control;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) fn evaluate(
    args: &[Value],
    matching: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            if matching {
                "jsonpath_match takes at least 2 args"
            } else {
                "jsonpath_exists takes at least 2 args"
            }
            .into(),
        ));
    }
    let root = input(&args[0], control)?;
    let path = value_to_string_with_control(&args[1], control)?;
    let path = path.trim();
    let path = path
        .strip_prefix("strict ")
        .or_else(|| path.strip_prefix("lax "))
        .unwrap_or(path)
        .trim();
    let result = if matching {
        match_value(&root, path, control)?
    } else {
        exists(&root, path, control)?
    };
    inline(Value::Bool(result), control)
}

fn exists(root: &Node, path: &str, control: &ProductionControl<'_>) -> Result<bool> {
    let (selector, filter) = if let Some(index) = find(path, "?", control)? {
        let filter = path[index + 1..].trim();
        let filter = if filter.starts_with('(') && filter.ends_with(')') {
            filter[1..filter.len() - 1].trim()
        } else {
            filter
        };
        (path[..index].trim(), Some(filter))
    } else {
        (path.trim(), None)
    };
    let values = select(root, selector, control)?;
    let Some(filter) = filter else {
        return Ok(!values.is_empty());
    };
    for value in values.iter() {
        if predicate(root, value, filter, control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn match_value(root: &Node, path: &str, control: &ProductionControl<'_>) -> Result<bool> {
    if let Some((left, op, right)) = comparison(path, control)? {
        let values = if left == "@" {
            single(root, control)?
        } else {
            select(root, left, control)?
        };
        return any_compare(&values, &literal(right, control)?, op, control);
    }
    Ok(matches!(
        select(root, path, control)?.first(),
        Some(Node::Bool(true))
    ))
}

fn single<'a>(node: &'a Node, control: &ProductionControl<'_>) -> Result<Produced<Vec<&'a Node>>> {
    let mut output = ProductionVec::new(*control);
    output.push_copy(node)?;
    Ok(output.finish()?)
}

fn select<'a>(
    root: &'a Node,
    selector: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<&'a Node>>> {
    control.check()?;
    let selector = selector.trim();
    let Some(mut rest) = selector.strip_prefix('$') else {
        return Err(SQLError::TypeMismatch(format!(
            "jsonpath selector must start with $, got {selector:?}"
        )));
    };
    let mut current = single(root, control)?;
    while !rest.is_empty() {
        control.check()?;
        let mut next_values = ProductionVec::new(*control);
        if let Some(next) = rest.strip_prefix('.') {
            let (key, after_key) = take_key(next, control)?;
            for value in current.iter() {
                control.check()?;
                if let Node::Object(fields) = value {
                    if let Some(index) = field_index(fields, key, control)? {
                        next_values.push_copy(&fields[index].value)?;
                    }
                }
            }
            rest = after_key;
        } else if let Some(next) = rest.strip_prefix("[*]") {
            for value in current.iter() {
                control.check()?;
                if let Node::Array(values) = value {
                    for value in values.iter() {
                        next_values.push_copy(value)?;
                    }
                }
            }
            rest = next;
        } else if let Some(next) = rest.strip_prefix('[') {
            let Some(end) = find(next, "]", control)? else {
                return Err(SQLError::TypeMismatch(format!(
                    "unterminated jsonpath array index in {selector:?}"
                )));
            };
            let index = &next[..end];
            for value in current.iter() {
                control.check()?;
                if let Node::Array(values) = value {
                    if let Some(index) = super::super::json_array_index(values.len(), index) {
                        next_values.push_copy(&values[index])?;
                    }
                }
            }
            rest = &next[end + 1..];
        } else {
            return Err(SQLError::TypeMismatch(format!(
                "unsupported jsonpath selector tail {rest:?}"
            )));
        }
        current = next_values.finish()?;
    }
    Ok(current)
}

fn take_key<'a>(input: &'a str, control: &ProductionControl<'_>) -> Result<(&'a str, &'a str)> {
    if let Some(quoted) = input.strip_prefix('"') {
        let Some(end) = find(quoted, "\"", control)? else {
            return Err(SQLError::TypeMismatch(
                "unterminated quoted jsonpath key".into(),
            ));
        };
        return Ok((&quoted[..end], &quoted[end + 1..]));
    }
    let mut end = input.len();
    for (index, ch) in input.char_indices() {
        control.check()?;
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            end = index;
            break;
        }
    }
    if end == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "expected jsonpath key in {input:?}"
        )));
    }
    Ok((&input[..end], &input[end..]))
}

fn predicate(
    root: &Node,
    current: &Node,
    predicate: &str,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    if let Some((left, op, right)) = comparison(predicate, control)? {
        let values = if left == "@" {
            single(current, control)?
        } else if let Some(selector) = left.strip_prefix('@') {
            select(
                current,
                &control.format(format_args!("${selector}"))?,
                control,
            )?
        } else {
            select(root, left, control)?
        };
        return any_compare(&values, &literal(right, control)?, op, control);
    }
    let values = if predicate == "@" {
        single(current, control)?
    } else {
        select(root, predicate, control)?
    };
    Ok(!values.is_empty())
}

type Comparison<'a> = (&'a str, &'static str, &'a str);

fn comparison<'a>(
    input: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<Comparison<'a>>> {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some(index) = find(input, op, control)? {
            return Ok(Some((
                input[..index].trim(),
                op,
                input[index + op.len()..].trim(),
            )));
        }
    }
    Ok(None)
}

fn find(input: &str, needle: &str, control: &ProductionControl<'_>) -> Result<Option<usize>> {
    // All grammar delimiters are ASCII. Byte scanning cannot split a valid matching delimiter and checks both tokens without allocating.
    for (index, bytes) in input.as_bytes().windows(needle.len()).enumerate() {
        if index.is_multiple_of(4096) {
            control.check()?;
        }
        if bytes == needle.as_bytes() {
            return Ok(Some(index));
        }
    }
    control.check()?;
    Ok(None)
}

fn literal(input: &str, control: &ProductionControl<'_>) -> Result<Node> {
    let input = input.trim();
    if let Some(value) = parsed::parse_optional(input, control)? {
        return Ok(value);
    }
    if input.starts_with('"') && input.ends_with('"') {
        return Ok(Node::String(control.copy_text(&input[1..input.len() - 1])?));
    }
    Err(SQLError::TypeMismatch(format!(
        "unsupported jsonpath literal {input:?}"
    )))
}

fn any_compare(
    values: &[&Node],
    right: &Node,
    op: &str,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    for left in values {
        control.check()?;
        if compare(left, right, op, control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn compare(left: &Node, right: &Node, op: &str, control: &ProductionControl<'_>) -> Result<bool> {
    control.check()?;
    let ordering = match (left, right) {
        (Node::Number(left), Node::Number(right)) => {
            let Some(left) = left.parse::<f64>().ok().filter(|value| value.is_finite()) else {
                return Ok(false);
            };
            let Some(right) = right.parse::<f64>().ok().filter(|value| value.is_finite()) else {
                return Ok(false);
            };
            left.partial_cmp(&right)
                .expect("finite JSON path numeric comparison")
        }
        (Node::String(left), Node::String(right)) => writer::compare_keys(left, right, control)?,
        (Node::Bool(left), Node::Bool(right)) => left.cmp(right),
        (Node::Null, Node::Null) => return Ok(matches!(op, "==" | ">=" | "<=")),
        _ => return Ok(op == "!="),
    };
    Ok(match op {
        "==" => ordering.is_eq(),
        "!=" => !ordering.is_eq(),
        ">" => ordering.is_gt(),
        ">=" => ordering.is_ge(),
        "<" => ordering.is_lt(),
        "<=" => ordering.is_le(),
        _ => false,
    })
}
