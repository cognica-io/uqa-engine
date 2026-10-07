//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite type declarations.

use pg_query::protobuf::{AlterTableStmt, AlterTableType, ColumnDef, CompositeTypeStmt};
use pg_query::NodeEnum;

use super::{
    domains::qualified_name, names::range_var_name, types::compile_pg_type_reference, Result,
    SQLError,
};
use crate::ast::{
    AlterTypeObject, AlterTypeObjectAction, CompositeAttributeAddition,
    CompositeAttributeDefinition, CreateCompositeType, Statement, TypeObjectKind,
};

pub(super) fn compile_composite_additions(statement: &AlterTableStmt) -> Result<Statement> {
    let relation = statement
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("composite attribute change has no relation".into()))?;
    let mut attributes = Vec::new();
    for node in &statement.cmds {
        let Some(NodeEnum::AlterTableCmd(command)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "composite attribute change has no command".into(),
            ));
        };
        if command.subtype() != AlterTableType::AtAddColumn {
            return Err(SQLError::Unsupported(format!(
                "ALTER TYPE attribute action {:?} is not supported",
                command.subtype()
            )));
        }
        let Some(NodeEnum::ColumnDef(column)) =
            command.def.as_ref().and_then(|node| node.node.as_ref())
        else {
            return Err(SQLError::Internal(
                "composite attribute change has no declaration".into(),
            ));
        };
        let declaration = super::tree::compile_column_declaration(column)?;
        let mut attribute = compile_attribute(column, true)?;
        if declaration.serial {
            attribute.ty = super::types::preserve_alter_type_declaration(
                column.type_name.as_ref().expect("compiled attribute type"),
                &column.colname,
            )?;
        }
        attributes.push(CompositeAttributeAddition {
            attribute,
            declaration,
        });
    }
    Ok(Statement::AlterTypeObject(AlterTypeObject {
        kind: TypeObjectKind::Type,
        name: range_var_name(relation),
        action: AlterTypeObjectAction::AddAttributes(attributes),
    }))
}

fn compile_attribute(column: &ColumnDef, retained: bool) -> Result<CompositeAttributeDefinition> {
    let type_name = column
        .type_name
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("attribute `{}` has no type", column.colname)))?;
    Ok(CompositeAttributeDefinition {
        name: column.colname.clone(),
        ty: if retained && !type_name.typmods.is_empty() {
            super::types::preserve_alter_type_declaration(type_name, &column.colname)?
        } else {
            compile_pg_type_reference(type_name, &column.colname)?
        },
        collation: column
            .coll_clause
            .as_ref()
            .map(|clause| qualified_name(&clause.collname))
            .transpose()?,
        setof: type_name.setof,
    })
}

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
            compile_attribute(column, false)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CreateCompositeType {
        name: range_var_name(relation),
        attributes,
    })
}
