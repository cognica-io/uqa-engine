//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute the selected catalog overload of unnest. Array calls also back the multi-array FROM syntax.

use super::{context::TableFunctionContext, values::unnest_rows, TableFunctionCall};
use crate::{PhysicalProjectRows, PhysicalRow, SQLRowStream};
use uqa_core::memory::ProductionVec;
use uqa_core::{ArrayValue, Value};
use uqa_sql::expr::{EngineValueCatalog, RangeComparisonValue};
use uqa_sql::{RowSchema, SQLError};

pub(super) fn rows(
    context: &TableFunctionContext<'_>,
    call: &TableFunctionCall<'_>,
    schema: &RowSchema,
    mut values: Vec<Value>,
) -> Result<PhysicalProjectRows, SQLError> {
    let signature = call
        .binding
        .map(|binding| binding.argument_types.as_slice());
    let Some([declared]) = signature else {
        return unnest_rows(values);
    };
    if !matches!(declared.as_str(), "tsvector" | "anymultirange") {
        return unnest_rows(values);
    }
    let Some(value) = values.pop() else {
        return Err(SQLError::Internal("selected unnest has no argument".into()));
    };
    if value == Value::Null {
        return Ok(SQLRowStream::new(std::iter::empty()));
    }
    let resources = context.routines.runtime.planning_resources()?;
    let control = resources.control();
    if declared == "tsvector" {
        let Value::Str(text) = value else {
            return Err(SQLError::Internal("expected admitted tsvector".into()));
        };
        let (entries, memory) = uqa_sql::tsvector::parse(&text, &control)?.into_parts();
        return Ok(SQLRowStream::new(entries.into_iter().map(move |entry| {
            let _retained = &memory;
            let (positions, weights) = if entry.positions.is_empty() {
                (Value::Null, Value::Null)
            } else {
                let (positions, weights) = entry
                    .positions
                    .into_iter()
                    .map(|(pos, weight)| {
                        (
                            Value::Int(i64::from(pos)),
                            Value::Str(char::from(b'D' - weight).to_string()),
                        )
                    })
                    .unzip();
                (array(positions, 21), array(weights, 25))
            };
            Ok(PhysicalRow::from_values(vec![
                Value::Str(entry.text),
                positions,
                weights,
            ]))
        })));
    }
    let argument = uqa_sql::scalar_call_argument(&call.args[0])?;
    let ty = uqa_sql::scalar_type_with_resolver_and_control(
        argument.value,
        schema,
        context.params,
        Some(context.resolver),
        &control,
    )?
    .ok_or_else(|| SQLError::Internal("missing selected multirange type".into()))?;
    let catalog = EngineValueCatalog(context.eval_hook);
    let members =
        RangeComparisonValue::from_type_with_control(&value, &ty, Some(&catalog), &control)?
            .ok_or_else(|| SQLError::Internal("expected admitted multirange".into()))?;
    let count = members
        .member_count()
        .ok_or_else(|| SQLError::Internal("expected a multirange".into()))?;
    if matches!(members, RangeComparisonValue::DeclaredMultirange(_)) {
        // Named bounds require the current statement's borrowed catalog for native encoding.
        let mut output = ProductionVec::new(control);
        for index in 0..count {
            output.push_produced(
                members
                    .member_value_with_control(index, Some(&catalog), &control)?
                    .expect("member index in range"),
            )?;
        }
        let (values, memory) = output.finish()?.into_parts();
        return Ok(SQLRowStream::new(values.into_iter().map(move |member| {
            let _retained = &memory;
            Ok(PhysicalRow::from_values(vec![member]))
        })));
    }
    Ok(SQLRowStream::new((0..count).map(move |index| {
        let member = members
            .member_value_with_control(index, None, &resources.control())?
            .expect("member index in range");
        Ok(PhysicalRow::from_values(vec![member.into_parts().0]))
    })))
}

fn array(elements: Vec<Value>, element_oid: u32) -> Value {
    Value::Array(
        ArrayValue::try_new(elements)
            .expect("one-dimensional lexeme metadata")
            .with_element_type_oid(Some(element_oid)),
    )
}
