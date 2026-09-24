//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{keys_equal, writer, Node};
use crate::error::Result;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

pub(super) fn compact(
    node: &Node,
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    writer::format_relaxed(node, jsonb, control)
}

pub(super) fn format(node: &Node, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write(node, 0, &mut output, control)?;
    Ok(output.finish()?)
}

fn indent(depth: usize, output: &mut ProductionString<'_>) -> Result<()> {
    for _ in 0..depth {
        output.push_str("    ")?;
    }
    Ok(())
}

fn write(
    node: &Node,
    depth: usize,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    control.check()?;
    match node {
        Node::Array(values) => {
            output.push_str("[\n")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push_str(",\n")?;
                }
                indent(depth + 1, output)?;
                write(value, depth + 1, output, control)?;
            }
            if !values.is_empty() {
                output.push('\n')?;
            }
            indent(depth, output)?;
            output.push(']')?;
        }
        Node::Object(fields) => {
            let order = writer::ordered(fields, true, control)?;
            output.push_str("{\n")?;
            let mut previous: Option<&str> = None;
            for (_, field) in &order.entries {
                control.check()?;
                if let Some(previous) = previous {
                    if keys_equal(previous, &field.key, control)? {
                        continue;
                    }
                    output.push_str(",\n")?;
                }
                indent(depth + 1, output)?;
                writer::write_string(&field.key, output, control)?;
                output.push_str(": ")?;
                write(&field.value, depth + 1, output, control)?;
                previous = Some(&field.key);
            }
            if previous.is_some() {
                output.push('\n')?;
            }
            indent(depth, output)?;
            output.push('}')?;
        }
        _ => writer::write(node, true, false, output, control)?,
    }
    Ok(())
}
