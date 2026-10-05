//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The clauses of a column definition as `transformColumnDefinition` reads them, which the statement keeps until the relation is found.

use super::{NodeEnum, Result, SQLError};
use crate::ast::{ColumnClause, ColumnClauseKind, ColumnDeclaration};
use pg_query::protobuf::ConstrType;

/// The pseudo-types `transformColumnDefinition` treats as SERIAL: one unqualified name, without `%TYPE`.
const SERIAL_TYPES: [&str; 6] = [
    "smallserial",
    "serial2",
    "serial",
    "serial4",
    "bigserial",
    "serial8",
];

/// The SERIAL type and the clauses, in written order, of a column definition.
pub(in crate::compiler) fn compile_column_declaration(
    column: &pg_query::protobuf::ColumnDef,
) -> Result<ColumnDeclaration> {
    let serial = column.type_name.as_ref().is_some_and(|type_name| {
        !type_name.pct_type
            && matches!(
                type_name.names.as_slice(),
                [name] if matches!(
                    name.node.as_ref(),
                    Some(NodeEnum::String(name)) if SERIAL_TYPES.contains(&name.sval.as_str())
                )
            )
    });
    let serial_array = serial
        && column
            .type_name
            .as_ref()
            .is_some_and(|type_name| !type_name.array_bounds.is_empty());
    let clauses = column
        .constraints
        .iter()
        .map(|node| {
            let Some(NodeEnum::Constraint(constraint)) = node.node.as_ref() else {
                return Err(SQLError::Internal(format!(
                    "unexpected column constraint node {:?}",
                    node.node
                )));
            };
            Ok(ColumnClause {
                kind: clause_kind(constraint.contype())?,
                name: (!constraint.conname.is_empty()).then(|| constraint.conname.clone()),
                no_inherit: constraint.is_no_inherit,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ColumnDeclaration {
        serial,
        serial_array,
        clauses,
    })
}

fn clause_kind(kind: ConstrType) -> Result<ColumnClauseKind> {
    Ok(match kind {
        ConstrType::ConstrNull => ColumnClauseKind::Null,
        ConstrType::ConstrNotnull => ColumnClauseKind::NotNull,
        ConstrType::ConstrDefault => ColumnClauseKind::Default,
        ConstrType::ConstrIdentity => ColumnClauseKind::Identity,
        ConstrType::ConstrGenerated => ColumnClauseKind::Generated,
        ConstrType::ConstrCheck => ColumnClauseKind::Check,
        ConstrType::ConstrPrimary => ColumnClauseKind::PrimaryKey,
        ConstrType::ConstrUnique => ColumnClauseKind::Unique,
        ConstrType::ConstrForeign => ColumnClauseKind::ForeignKey,
        ConstrType::ConstrAttrDeferrable => ColumnClauseKind::Deferrable,
        ConstrType::ConstrAttrNotDeferrable => ColumnClauseKind::NotDeferrable,
        ConstrType::ConstrAttrDeferred => ColumnClauseKind::InitiallyDeferred,
        ConstrType::ConstrAttrImmediate => ColumnClauseKind::InitiallyImmediate,
        ConstrType::ConstrAttrEnforced => ColumnClauseKind::Enforced,
        ConstrType::ConstrAttrNotEnforced => ColumnClauseKind::NotEnforced,
        other => {
            return Err(SQLError::Unsupported(format!(
                "column constraint {other:?} is not supported"
            )))
        }
    })
}
