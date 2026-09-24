//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Field, Node};
use crate::{
    error::{Result, SQLError},
    expr::out_of_range,
};
use uqa_core::{memory::MemoryReservation, ordering::sort_by_with_control};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    DecimalValue, ValueRetentionError,
};

// Field order keeps the vector alive under its allowance through errors and unwinding.
pub(super) struct ObjectOrder<T> {
    pub(super) entries: Vec<T>,
    _memory: Option<MemoryReservation>,
}

pub(super) fn format(
    node: &Node,
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write(node, jsonb, true, &mut output, control)?;
    Ok(output.finish()?)
}

pub(super) fn write(
    node: &Node,
    jsonb: bool,
    validate_numeric: bool,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    control.check()?;
    match node {
        Node::Null => output.push_str("null")?,
        Node::Bool(value) => output.push_str(if *value { "true" } else { "false" })?,
        Node::Number(text) => {
            if jsonb {
                if let Some(decimal) = DecimalValue::parse_with_control(text, control)? {
                    output.push_str(&decimal.to_sql_string_with_control(control)?)?;
                } else if validate_numeric {
                    return Err(out_of_range("numeric"));
                } else {
                    output.push_str(text)?;
                }
            } else {
                output.push_str(text)?;
            }
        }
        Node::String(text) => write_string(text, output, control)?,
        Node::Array(values) => {
            output.push('[')?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push_str(if jsonb { ", " } else { "," })?;
                }
                write(value, jsonb, validate_numeric, output, control)?;
            }
            output.push(']')?;
        }
        Node::Object(fields) => write_object(fields, jsonb, validate_numeric, output, control)?,
    }
    Ok(())
}

fn write_object(
    fields: &[Field],
    jsonb: bool,
    validate_numeric: bool,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    let order = ordered(fields, jsonb, control)?;
    output.push('{')?;
    let mut previous: Option<&str> = None;
    for (_, field) in &order.entries {
        if let Some(previous) = previous {
            if compare_keys(previous, &field.key, control)?.is_eq() {
                continue;
            }
            output.push_str(if jsonb { ", " } else { "," })?;
        }
        write_string(&field.key, output, control)?;
        output.push_str(if jsonb { ": " } else { ":" })?;
        write(&field.value, jsonb, validate_numeric, output, control)?;
        previous = Some(&field.key);
    }
    output.push('}')?;
    drop(order);
    Ok(())
}

pub(super) fn ordered<'a>(
    fields: &'a [Field],
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<ObjectOrder<(usize, &'a Field)>> {
    let mut entries = ProductionVec::new(*control);
    for (index, field) in fields.iter().enumerate() {
        entries.push_copy((index, field))?;
    }
    let (entries, memory) = entries.finish()?.into_parts();
    let mut order = ObjectOrder {
        entries,
        _memory: memory,
    };
    let mut poll = || control.check().map_err(SQLError::from);
    sort_by_with_control(
        &mut order.entries,
        &mut poll,
        |(left_index, left), (right_index, right), _| {
            let order = if jsonb {
                left.key.len().cmp(&right.key.len())
            } else {
                std::cmp::Ordering::Equal
            };
            Ok(order
                .then(compare_keys(&left.key, &right.key, control)?)
                .then_with(|| right_index.cmp(left_index)))
        },
    )?;
    Ok(order)
}

pub(super) fn format_relaxed(
    node: &Node,
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write(node, jsonb, false, &mut output, control)?;
    Ok(output.finish()?)
}

pub(super) fn compare_keys(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> Result<std::cmp::Ordering> {
    Ok(super::compare_keys(left, right, control)?)
}

pub(super) fn scalar<T: serde::Serialize + ?Sized>(
    value: &T,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write_scalar(value, &mut output)?;
    Ok(output.finish()?)
}

pub(super) fn write_string(
    text: &str,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    output.push('"')?;
    let mut start = 0;
    while start < text.len() {
        control.check()?;
        let mut end = start.saturating_add(4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        // String escaping is compositional at UTF-8 boundaries. Bound serde's scan as well as its destination writes, retaining the admitted scratch until its interior is copied.
        let escaped = scalar(&text[start..end], control)?;
        output.push_str(&escaped[1..escaped.len() - 1])?;
        start = end;
    }
    output.push('"')?;
    Ok(())
}

/// serde's scalar serializer supplies its existing string escaping and finite-float spelling directly to the admitted UTF-8 destination; no serde String/Number carrier is constructed.
fn write_scalar<T: serde::Serialize + ?Sized>(
    value: &T,
    output: &mut ProductionString<'_>,
) -> Result<()> {
    let mut writer = Writer {
        output,
        error: None,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if let Some(error) = writer.error {
        return Err(error.into());
    }
    result.map_err(|error| SQLError::Internal(format!("JSON scalar serialization failed: {error}")))
}

struct Writer<'a, 'c> {
    output: &'a mut ProductionString<'c>,
    error: Option<ValueRetentionError>,
}

impl std::io::Write for Writer<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(bytes)
            .expect("JSON scalar serializer writes complete UTF-8 fragments");
        self.output.push_str(text).map_err(|error| {
            self.error = Some(error);
            std::io::Error::from(std::io::ErrorKind::Other)
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::expr) fn quote_with_control(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write_string(text, &mut output, control)?;
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    struct DropWitness<'a> {
        budget: &'a MemoryBudget,
        observed: &'a Cell<bool>,
    }

    impl Drop for DropWitness<'_> {
        fn drop(&mut self) {
            self.observed.set(self.budget.used() != 0);
        }
    }

    #[test]
    fn object_order_releases_values_before_their_lease_on_error_and_unwind() {
        let memory = MemoryBudget::new(4096);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancellation, &cancellation);
        let observed = Cell::new(false);
        let make_order = || {
            let mut entries = ProductionVec::new(control);
            entries
                .push_produced(
                    control
                        .finish(
                            DropWitness {
                                budget: &memory,
                                observed: &observed,
                            },
                            control.empty_reservation(),
                        )
                        .unwrap(),
                )
                .unwrap();
            let (entries, memory) = entries.finish().unwrap().into_parts();
            ObjectOrder {
                entries,
                _memory: memory,
            }
        };
        let failed = (|| -> std::result::Result<(), ()> {
            let _owner = make_order();
            let result = std::result::Result::<(), ()>::Err(());
            result?;
            Ok(())
        })();
        assert!(failed.is_err());
        assert!(observed.get());
        assert_eq!(memory.used(), 0);
        observed.set(false);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _owner = make_order();
            panic!("injected JSON order failure");
        }));
        assert!(unwound.is_err());
        assert!(observed.get());
        assert_eq!(memory.used(), 0);
    }
}
