//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve the selected B-tree class and exact support signatures before allocating any type OIDs.

use super::{error, RangeCreationContext};
use crate::catalog::projection;
use uqa_core::{memory::ProductionControl, RelationIdentity, Value};
use uqa_sql::{
    ast::FunctionVolatility,
    catalog::index::operator_classes::{
        builtin_btree_operator_classes, default_btree_operator_class,
    },
    ColumnType, SQLError,
};

pub(super) fn operator_class(
    context: &RangeCreationContext<'_>,
    subtype: &ColumnType,
    name: Option<&str>,
) -> Result<u32, SQLError> {
    if let Some(name) = name {
        let (schema, local) =
            RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        if let Some(schema) = &schema {
            if context
                .catalog
                .schema_security_for_privilege(schema)
                .is_none()
            {
                return Err(error(
                    "3F000",
                    format!("schema \"{schema}\" does not exist"),
                ));
            }
            context.catalog.require_schema_privilege(
                schema,
                &context.catalog.current_role(),
                crate::catalog::security::schema::SchemaAclPrivilege::Usage,
            )?;
        }
        let class = builtin_btree_operator_classes()
            .iter()
            .find(|class| {
                class.name == local
                    && schema
                        .as_deref()
                        .is_none_or(|schema| schema == "pg_catalog")
            })
            .ok_or_else(|| {
                error(
                    "42704",
                    format!("operator class \"{name}\" does not exist for access method \"btree\""),
                )
            })?;
        class.bind_input(subtype, name, &ProductionControl::uncontrolled())?;
        Ok(class.oid)
    } else {
        let display = context
            .routines
            .definition
            .compilation
            .analysis
            .types
            .format_type(subtype)?;
        default_btree_operator_class(subtype).map(|class| class.oid).ok_or_else(|| SQLError::Diagnostic {
            sqlstate: "42704".into(), message: format!("data type {display} has no default operator class for access method \"btree\""),
            detail: None, hint: Some("You must specify an operator class for the range type or define a default operator class for the subtype.".into()),
        })
    }
}

pub(super) fn routine(
    context: &RangeCreationContext<'_>,
    name: &str,
    input: u32,
    canonical: bool,
) -> Result<u32, SQLError> {
    let (schema, local) = RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
    let arguments = [i64::from(input); 2];
    let arguments = &arguments[..if canonical { 1 } else { 2 }];
    let types = context.routines.definition.compilation.analysis.types;
    let display = types.format_type_oid(input)?;
    let signature = if canonical {
        format!("{name}({display})")
    } else {
        format!("{name}({display}, {display})")
    };
    let oid = projection::routine_oid_for_arguments(
        &context.catalog,
        schema.as_deref(),
        &local,
        arguments,
    )?
    .ok_or_else(|| error("42883", format!("function {signature} does not exist")))?;
    let catalog = context.catalog.catalog_read_view();
    let oid = u32::try_from(oid).expect("routine OID");
    let (result, immutable) = if let Some(routine) = catalog.sql_function_by_oid(oid)? {
        (
            projection::catalog_routine_type_oid(
                &catalog,
                uqa_sql::routines::declaration::result_type_name(&routine.def),
            ),
            routine.def.volatility == FunctionVolatility::Immutable,
        )
    } else {
        let routine = projection::builtin_routine_by_oid(i64::from(oid))
            .ok_or_else(|| SQLError::Internal(format!("missing routine {oid}")))?;
        (routine.return_type, routine.volatility == "i")
    };
    let kind = if canonical {
        "canonical"
    } else {
        "subtype diff"
    };
    if result != if canonical { i64::from(input) } else { 701 } {
        let expected = if canonical {
            "range type"
        } else {
            "type double precision"
        };
        return Err(error(
            "42P17",
            format!("range {kind} function {signature} must return {expected}"),
        ));
    }
    if !immutable {
        return Err(error(
            "42P17",
            format!("range {kind} function {signature} must be immutable"),
        ));
    }
    let permitted = crate::catalog::security::routine_inquiry::has_function_privilege_value(
        &context.catalog,
        &[Value::Int(i64::from(oid)), Value::Str("EXECUTE".into())],
    )?;
    if permitted != Value::Bool(true) {
        return Err(error(
            "42501",
            format!("permission denied for function {local}"),
        ));
    }
    Ok(oid)
}
