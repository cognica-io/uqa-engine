//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected scalar syntax retains its resolved record and integer identity in admitted replacement metadata.

use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{plan::OwnedScalarExpr, ColumnType, SQLError, ScalarExpr};

pub(in crate::optimizer) fn retain_record_result(
    expression: OwnedScalarExpr,
    row_syntax: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if !row_syntax && matches!(&*expression, ScalarExpr::Row(_)) {
        Ok(OwnedScalarExpr::cast(
            expression,
            ColumnType::Record.catalog_name_with_control(control)?,
            true,
            control,
        )?)
    } else {
        Ok(expression)
    }
}

pub(in crate::optimizer) fn retain_computed_integer(
    expression: OwnedScalarExpr,
    parameter_index: Option<usize>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Literal(Value::Int(value)) = &*expression else {
        return Ok(expression);
    };
    let ty = match uqa_sql::expr::integer_width_for_literal(*value) {
        uqa_sql::expr::IntegerWidth::SmallInt => ColumnType::SmallInteger,
        uqa_sql::expr::IntegerWidth::Integer => ColumnType::Integer,
        uqa_sql::expr::IntegerWidth::BigInt => ColumnType::BigInteger,
    };
    let declaration = ty.catalog_name_with_control(control)?;
    let ty = control.finish(ty, control.empty_reservation())?;
    Ok(OwnedScalarExpr::typed_literal(
        expression.into_literal_value(control)?,
        declaration,
        Some(ty),
        None,
        parameter_index,
        control,
    )?)
}
