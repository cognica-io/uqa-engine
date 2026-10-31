//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal and reference compilation reads parser-owned inputs and admits only the surviving SQL syntax fields.

use super::{Expr, NodeEnum, Result, SQLError, Value};
use crate::ast::ReferenceName;
use pg_query::protobuf::{a_const::Val, AConst, ColumnRef};
use uqa_core::{
    memory::{Produced, ProductionControl},
    DecimalValue,
};

pub(in crate::compiler) fn compile_const_with_control(
    constant: &AConst,
    control: &ProductionControl<'_>,
) -> Result<Produced<Expr>> {
    control.check()?;
    if constant.isnull {
        if constant.val.is_some() {
            return Err(SQLError::Internal(
                "NULL constant unexpectedly has a value payload".into(),
            ));
        }
        return Ok(control.finish(Expr::Literal(Value::Null), control.empty_reservation())?);
    }
    let value = match constant
        .val
        .as_ref()
        .ok_or_else(|| SQLError::Internal("non-NULL constant has no value payload".into()))?
    {
        Val::Ival(integer) => control.finish(
            Value::Int(i64::from(integer.ival)),
            control.empty_reservation(),
        )?,
        Val::Fval(number) => numeric(&number.fval, control)?,
        Val::Sval(string) => {
            let (text, memory) = control.copy_text(&string.sval)?.into_parts();
            control.finish(Value::Str(text), memory)?
        }
        Val::Bsval(bits) => {
            let (bits, memory) = crate::expr::bit_string::input_with_control(
                &bits.bsval,
                crate::expr::bit_string::BitStringKind::Fixed,
                -1,
                control,
            )?
            .into_parts();
            control.finish(Value::BitString(bits), memory)?
        }
        Val::Boolval(boolean) => {
            control.finish(Value::Bool(boolean.boolval), control.empty_reservation())?
        }
    };
    let (value, memory) = value.into_parts();
    Ok(control.finish(Expr::Literal(value), memory)?)
}

fn numeric(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let integral = text
        .strip_prefix('-')
        .unwrap_or(text)
        .bytes()
        .all(|byte| byte.is_ascii_digit());
    if integral {
        if let Ok(integer) = text.parse::<i64>() {
            return Ok(control.finish(Value::Int(integer), control.empty_reservation())?);
        }
    }
    if let Some(number) = DecimalValue::parse_with_control(text, control)? {
        let (number, memory) = number.into_parts();
        return Ok(control.finish(Value::Decimal(number), memory)?);
    }
    if integral {
        let message = control.format(format_args!("bad numeric literal {text}"))?;
        return Err(SQLError::Internal(message.into_parts().0));
    }
    match text.parse::<f64>() {
        Ok(number) => Ok(control.finish(Value::Float(number), control.empty_reservation())?),
        Err(error) => {
            let message = control.format(format_args!("{error}"))?;
            Err(SQLError::Internal(message.into_parts().0))
        }
    }
}

pub(in crate::compiler) fn compile_column_ref_with_control(
    reference: &ColumnRef,
    control: &ProductionControl<'_>,
) -> Result<Produced<Expr>> {
    control.check()?;
    let position = crate::compiler::source_positions::position(reference.location);
    let mut qualifier = None;
    let mut last = None;
    for (index, field) in reference.fields.iter().enumerate() {
        control.check()?;
        let inner = field
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ColumnRef contains an empty field".into()))?;
        match inner {
            NodeEnum::String(string) if !string.sval.is_empty() => {
                qualifier = last;
                last = Some(string.sval.as_str());
            }
            NodeEnum::String(_) => {
                return Err(SQLError::Internal(
                    "ColumnRef contains an empty name component".into(),
                ))
            }
            NodeEnum::AStar(_) if reference.fields.len() == 1 && index == 0 => {
                return Ok(control.finish(Expr::Star, control.empty_reservation())?);
            }
            NodeEnum::AStar(_) => {
                let last = last.ok_or_else(|| {
                    SQLError::Internal("qualified wildcard has no relation name".into())
                })?;
                let (name, memory) = control.copy_text(last)?.into_parts();
                return Ok(control.finish(
                    Expr::QualifiedStar(ReferenceName::from(name).with_position(position)),
                    memory,
                )?);
            }
            other => {
                let message = control.format(format_args!(
                    "ColumnRef contains unexpected field {other:?}"
                ))?;
                return Err(SQLError::Internal(message.into_parts().0));
            }
        }
    }
    let last = last.ok_or_else(|| SQLError::Internal("empty ColumnRef".into()))?;
    let column = control.copy_text(last)?;
    if let Some(qualifier) = qualifier {
        let qualifier = control.copy_text(qualifier)?;
        let (column, memory) = column.into_parts();
        let (qualifier, qualifier_memory) = qualifier.into_parts();
        Ok(control.finish(
            Expr::qualified_column(
                qualifier,
                ReferenceName::from(column).with_position(position),
            ),
            control.combine(memory, qualifier_memory),
        )?)
    } else {
        let (column, memory) = column.into_parts();
        Ok(control.finish(
            Expr::Column(ReferenceName::from(column).with_position(position)),
            memory,
        )?)
    }
}
