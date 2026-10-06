//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze a foreign-table definition in declaration order after execution resolves its creation namespace.

use super::{range_var_name, NodeEnum, Result, SQLError};
use crate::ast::{ColumnType, CreateForeignTable, DeferredCreateForeignTable};
use crate::schema::table_creation::column_declarations::{
    check_foreign_column_declaration, check_serial_array, foreign_table_constraint_error,
};
use crate::type_resolution::{resolve_declared_column_type, FunctionTypeResolver};

/// Resolve each column's type and clauses, and each table constraint, in written order. Execution calls this only after checking the creation namespace and an `IF NOT EXISTS` target.
pub fn resolve_deferred_create_foreign_table(
    deferred: &DeferredCreateForeignTable,
    types: &dyn FunctionTypeResolver,
) -> Result<CreateForeignTable> {
    let parsed = crate::parser::parse(&deferred.definition_sql)?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return Err(SQLError::Internal(
            "deferred CREATE FOREIGN TABLE did not contain exactly one statement".into(),
        ));
    };
    let node = raw
        .stmt
        .as_deref()
        .and_then(|node| node.node.as_ref())
        .ok_or_else(|| SQLError::Internal("deferred CREATE FOREIGN TABLE is empty".into()))?;
    let NodeEnum::CreateForeignTableStmt(statement) = node else {
        return Err(SQLError::Internal(
            "deferred CREATE FOREIGN TABLE changed statement kind".into(),
        ));
    };
    let base = statement
        .base_stmt
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without base".into()))?;
    let relation = base
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without relation".into()))?;
    if base.if_not_exists != deferred.if_not_exists
        || range_var_name(relation) != deferred.name
        || statement.servername != deferred.server_name
    {
        return Err(SQLError::Internal(
            "deferred CREATE FOREIGN TABLE changed target identity".into(),
        ));
    }
    super::validate_create_table_envelope(base, "CREATE FOREIGN TABLE")?;
    let column_types = analyze_elements(base, types, &relation.relname)?;
    // The supported declaration is lowered only after its ordered checks; ordinary-table key lowering must never preempt a foreign-table diagnostic.
    let mut table = super::relations::compile_create_foreign_table(statement)?;
    if column_types.len() != table.columns.len() {
        return Err(SQLError::Internal(
            "foreign-table declaration lost an analyzed column".into(),
        ));
    }
    for (column, ty) in table.columns.iter_mut().zip(column_types) {
        column.ty = ty;
    }
    Ok(table)
}

fn analyze_elements(
    statement: &pg_query::protobuf::CreateStmt,
    types: &dyn FunctionTypeResolver,
    table: &str,
) -> Result<Vec<ColumnType>> {
    use pg_query::protobuf::ConstrType;
    let mut column_types = Vec::new();
    for element in &statement.table_elts {
        match element.node.as_ref() {
            Some(NodeEnum::ColumnDef(column)) => {
                let declaration = super::tree::compile_column_declaration(column)?;
                check_serial_array(&declaration)?;
                let ty = super::types::compile_type_name(column)?;
                let ty = resolve_declared_column_type(types, &ty)?;
                check_foreign_column_declaration(&declaration, &column.colname, table)?;
                column_types.push(ty);
            }
            Some(NodeEnum::Constraint(constraint)) => {
                let kind = match constraint.contype() {
                    ConstrType::ConstrPrimary => "primary key",
                    ConstrType::ConstrUnique => "unique",
                    ConstrType::ConstrForeign => "foreign key",
                    ConstrType::ConstrExclusion => "exclusion",
                    ConstrType::ConstrCheck | ConstrType::ConstrNotnull => continue,
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "table constraint {other:?} is not supported"
                        )));
                    }
                };
                return Err(foreign_table_constraint_error(kind));
            }
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE FOREIGN TABLE element {other:?} is not supported"
                )));
            }
            None => {
                return Err(SQLError::Internal(
                    "CREATE FOREIGN TABLE contains an empty element".into(),
                ));
            }
        }
    }
    Ok(column_types)
}

pub(super) fn not_null_declarations(
    table: &crate::ast::CreateTable,
) -> Result<Vec<crate::ast::NotNullDeclaration>> {
    use crate::ast::{DeclaredElement, NotNullDeclaration};
    let mut declarations = Vec::new();
    let mut columns = table.columns.iter();
    for element in &table.element_order {
        match element {
            DeclaredElement::Column(declaration) => {
                let column = columns.next().ok_or_else(|| {
                    SQLError::Internal("foreign-table declaration lost a column".into())
                })?;
                let before = declarations.len();
                declarations.extend(
                    declaration
                        .clauses
                        .iter()
                        .filter(|clause| clause.kind == crate::ast::ColumnClauseKind::NotNull)
                        .map(|clause| NotNullDeclaration {
                            column: column.name.clone(),
                            name: clause.name.clone(),
                            no_inherit: clause.no_inherit,
                            explicit: true,
                        }),
                );
                if column.not_null && declarations.len() == before {
                    declarations.push(NotNullDeclaration {
                        column: column.name.clone(),
                        name: None,
                        no_inherit: false,
                        explicit: false,
                    });
                }
            }
            DeclaredElement::NotNull(declaration) => declarations.push(declaration.clone()),
            DeclaredElement::PrimaryKey { .. } | DeclaredElement::DeferrableKey => {
                return Err(SQLError::Internal(
                    "foreign-table declaration retained an unsupported key".into(),
                ));
            }
        }
    }
    Ok(declarations)
}

/// Retain a foreign-column deletion list in written order before catalog binding.
pub(super) fn column_drops(
    commands: &[pg_query::protobuf::Node],
) -> Option<Vec<crate::ast::DropColumnAction>> {
    if commands.is_empty() {
        return None;
    }
    commands
        .iter()
        .map(|node| {
            let Some(NodeEnum::AlterTableCmd(command)) = node.node.as_ref() else {
                return None;
            };
            (command.subtype() == pg_query::protobuf::AlterTableType::AtDropColumn).then(|| {
                crate::ast::DropColumnAction {
                    name: command.name.clone(),
                    if_exists: command.missing_ok,
                    cascade: matches!(
                        command.behavior(),
                        pg_query::protobuf::DropBehavior::DropCascade
                    ),
                }
            })
        })
        .collect()
}
