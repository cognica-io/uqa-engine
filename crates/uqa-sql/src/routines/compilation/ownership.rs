//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzed routine bodies retain selected type names without detaching their executable constructor owners.

use crate::{
    ast::UserTypeIdentity, plan::MutableExecutablePlan, routines::declaration::RoutineTypeCatalog,
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub fn bind_analyzed_sql_body_types_owned(
    types: &dyn RoutineTypeCatalog,
    plan: MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    plan.transform_scalar_nodes(control, &mut |_, expression| {
        let name = match &*expression {
            ScalarExpr::Cast { ty, .. }
            | ScalarExpr::TypedLiteral { ty, .. }
            | ScalarExpr::CompositeRow {
                binding: crate::ast::CompositeRowBinding { ty, .. },
                ..
            } => ty,
            _ => return Ok(expression),
        };
        if UserTypeIdentity::parse(name).is_some() {
            return Ok(expression);
        }
        let Some(ty) = types.resolve_catalog_column_type_with_control(name, control)? else {
            return Ok(expression);
        };
        let mut element = &*ty;
        while let ColumnType::Array(inner) = element {
            control.check()?;
            element = inner;
        }
        if matches!(element, ColumnType::Domain { .. }) {
            return Ok(expression);
        }
        let selected = ty.catalog_name_with_control(control)?;
        if selected.as_str() == name {
            return Ok(expression);
        }
        Ok(expression.replace_type_name(selected, control)?)
    })
}
