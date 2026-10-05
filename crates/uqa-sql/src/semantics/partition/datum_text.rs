//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text of stored partition bound datums, as `PostgreSQL`'s `get_const_expr` renders a constant without a type label: the value's output text quoted as a literal, except non-negative `integer`, float-looking `numeric`, and `boolean` values.

use crate::ast::{ColumnType, Expr, PartitionRangeDatum};
use crate::expr::EngineHook;
use crate::result::format_postgres_text;
use crate::SQLError;
use uqa_core::Value;

/// Render one bound datum of partition key type `ty`.
pub fn partition_datum_text(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    if matches!(value, Value::Null) {
        return Ok("NULL".into());
    }
    let text = format_postgres_text(value, ty, engine)?;
    Ok(match ty {
        ColumnType::Integer if !text.starts_with('-') => text,
        ColumnType::Numeric { .. }
            if text.starts_with(|character: char| character.is_ascii_digit())
                && text.contains(['e', 'E', '.']) =>
        {
            text
        }
        ColumnType::Boolean => if text == "t" { "true" } else { "false" }.into(),
        _ => quote_literal(&text),
    })
}

/// `get_range_partbound_string`: one range bound as `(datum, ...)`, with `MINVALUE` and `MAXVALUE` spelled out.
pub fn range_bound_text(
    datums: &[PartitionRangeDatum],
    types: &[ColumnType],
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    let mut rendered = Vec::with_capacity(datums.len());
    for (position, datum) in datums.iter().enumerate() {
        rendered.push(match datum {
            PartitionRangeDatum::MinValue => "MINVALUE".into(),
            PartitionRangeDatum::MaxValue => "MAXVALUE".into(),
            PartitionRangeDatum::Value(expression) => {
                let ty = types.get(position).ok_or_else(|| {
                    SQLError::Internal("partition range bound is wider than its key".into())
                })?;
                partition_datum_text(stored_datum(expression)?, ty, engine)?
            }
        });
    }
    Ok(format!("({})", rendered.join(", ")))
}

/// The constant a transformed bound stores for one datum.
pub fn stored_datum(expression: &Expr) -> Result<&Value, SQLError> {
    match expression {
        Expr::Literal(value) | Expr::TypedLiteral { value, .. } => Ok(value),
        other => Err(SQLError::Internal(format!(
            "partition bound datum was not evaluated to a constant: {other:?}"
        ))),
    }
}

/// `simple_quote_literal` with standard-conforming strings: only single quotes are doubled.
fn quote_literal(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        if character == '\'' {
            quoted.push('\'');
        }
        quoted.push(character);
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::partition_datum_text;
    use crate::ast::ColumnType;
    use uqa_core::{DecimalValue, Value};

    fn text(value: Value, ty: &ColumnType) -> String {
        partition_datum_text(&value, ty, None).unwrap()
    }

    #[test]
    fn constants_follow_get_const_expr_spelling() {
        assert_eq!(text(Value::Null, &ColumnType::Integer), "NULL");
        assert_eq!(text(Value::Int(2), &ColumnType::Integer), "2");
        assert_eq!(text(Value::Int(-1), &ColumnType::Integer), "'-1'");
        assert_eq!(text(Value::Int(2), &ColumnType::SmallInteger), "'2'");
        assert_eq!(text(Value::Int(2), &ColumnType::BigInteger), "'2'");
        let numeric = ColumnType::Numeric {
            precision: None,
            scale: None,
        };
        let decimal = |text: &str| Value::Decimal(DecimalValue::parse(text).unwrap());
        assert_eq!(text(decimal("1.50"), &numeric), "1.50");
        assert_eq!(text(decimal("2"), &numeric), "'2'");
        assert_eq!(text(decimal("-3.5"), &numeric), "'-3.5'");
        assert_eq!(text(Value::Bool(true), &ColumnType::Boolean), "true");
        assert_eq!(
            text(Value::Str("it's".into()), &ColumnType::Text),
            "'it''s'"
        );
        assert_eq!(text(Value::Str("x\\y".into()), &ColumnType::Text), "'x\\y'");
    }
}
