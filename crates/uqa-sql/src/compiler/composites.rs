//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite type declarations.

use pg_query::protobuf::CompositeTypeStmt;
use pg_query::NodeEnum;

use super::{
    domains::qualified_name, names::range_var_name, types::compile_pg_type_reference, Result,
    SQLError,
};
use crate::ast::{CompositeAttributeDefinition, CreateCompositeType};

/// `CREATE TYPE name AS (...)`. A catalog-qualified name must name the current database, as `RangeVarGetCreationNamespace` requires.
pub(super) fn compile_create_composite_type(
    statement: &CompositeTypeStmt,
) -> Result<CreateCompositeType> {
    let relation = statement
        .typevar
        .as_ref()
        .ok_or_else(|| SQLError::Internal("composite type declaration has no name".into()))?;
    if !relation.catalogname.is_empty() && relation.catalogname != crate::catalog::DATABASE_NAME {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cross-database references are not implemented: {}.{}.{}",
                relation.catalogname, relation.schemaname, relation.relname
            ),
        });
    }
    let attributes = statement
        .coldeflist
        .iter()
        .map(|node| {
            let Some(NodeEnum::ColumnDef(column)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "composite type declaration contains a malformed attribute".into(),
                ));
            };
            Ok(CompositeAttributeDefinition {
                name: column.colname.clone(),
                // Serial types exist only in table column declarations, so a composite attribute resolves the name as an ordinary type.
                ty: compile_pg_type_reference(
                    column.type_name.as_ref().ok_or_else(|| {
                        SQLError::Internal(format!("attribute `{}` has no type", column.colname))
                    })?,
                    &column.colname,
                )?,
                collation: column
                    .coll_clause
                    .as_ref()
                    .map(|clause| qualified_name(&clause.collname))
                    .transpose()?,
                setof: column
                    .type_name
                    .as_ref()
                    .is_some_and(|type_name| type_name.setof),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CreateCompositeType {
        name: range_var_name(relation),
        attributes,
    })
}
