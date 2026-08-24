//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{
    coerce_to_column_type, ddl_storage_error, eval_lowered_expression, index_vectors_for_type,
    rewrite_column_values_to_type, AlterTableAction, AlterTableStmt, BTreeMap, ColumnType,
    Document, Engine, RowUpdateVectors, SQLError, SQLResult, Value,
};
use uqa_sql::ast::{GeneratedColumn, GeneratedColumnKind};

use super::defaults::validate_default_expression;

mod constraint_drop;

use constraint_drop::{drop_column_cascade, drop_column_restrict, drop_constraint};

pub(in crate::sql) fn run_alter_table(
    engine: &Engine,
    mut stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    match engine
        .try_resolve_relation_kind(&stmt.table)
        .map_err(|err| ddl_storage_error("ALTER TABLE", err))?
    {
        Some((canonical, "table")) => stmt.table = canonical,
        Some((canonical, kind)) => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{canonical}` is a {kind}, not a table"
            )));
        }
        None if stmt.if_exists => return Ok(SQLResult::empty()),
        None => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{}` does not exist",
                stmt.table
            )));
        }
    }
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::AccessExclusive,
    )?;
    engine.with_implicit_transaction(move |engine| run_alter_table_inner(engine, stmt))
}

fn run_alter_table_inner(engine: &Engine, stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    let AlterTableStmt {
        table,
        qualifier,
        if_exists,
        actions,
    } = stmt;
    for action in actions {
        run_alter_table_action(
            engine,
            AlterTableStmt {
                table: table.clone(),
                qualifier: qualifier.clone(),
                if_exists,
                actions: Vec::new(),
            },
            action,
        )?;
    }
    Ok(SQLResult::empty())
}

fn run_alter_table_action(
    engine: &Engine,
    stmt: AlterTableStmt,
    action: AlterTableAction,
) -> Result<(), SQLError> {
    match action {
        AlterTableAction::AddColumn {
            mut column,
            if_not_exists,
        } => {
            let col_name = column.name.clone();
            if engine
                .try_table_has_column(&stmt.table, &col_name)
                .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
            {
                if if_not_exists {
                    return Ok(());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ADD COLUMN: column `{col_name}` already exists"
                )));
            }
            if let Some(default) = &column.default {
                validate_default_expression(engine, default)?;
            }
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            candidate_columns.push(column.clone());
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            column.generated = candidate_columns
                .last()
                .and_then(|candidate| candidate.generated.clone());
            let generated_kind = column.generated.as_ref().map(|generated| generated.kind);
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .create_vector_field(&stmt.table, col_name.clone(), dim)
                        .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
                }
                ColumnType::Text if generated_kind != Some(GeneratedColumnKind::Virtual) => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, col_name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                _ => {}
            }
            // Capture the default expression and NOT NULL flag before
            // moving the column into the engine so we can backfill any
            // existing rows. PostgreSQL evaluates the default once per
            // existing row at ALTER TABLE time, which keeps NOT NULL
            // constraints satisfiable for non-empty tables.
            let column_not_null = column.not_null;
            engine
                .try_register_column(&stmt.table, column)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
            if let Some(kind) = generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            } else {
                let default_expr = engine
                    .try_column_default_expr(&stmt.table, &col_name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN default", e))?;
                backfill_added_column(
                    engine,
                    &stmt.table,
                    &col_name,
                    default_expr.as_ref(),
                    column_not_null,
                )?;
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
        }
        AlterTableAction::AddKeyConstraint { constraint } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let declared_constraints = engine
                .try_declared_table_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            ensure_constraint_name_available(
                &columns,
                &declared_constraints,
                constraint.name.as_deref(),
                &stmt.table,
            )?;
            let mut key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            key_constraints.push(constraint.clone());
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            validate_added_key_constraint(engine, &stmt.table, &constraint)?;
            engine
                .add_key_constraint(&stmt.table, &constraint)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            add_check_constraint(engine, &stmt.table, constraint)?;
        }
        AlterTableAction::AddForeignKeyConstraint { constraint } => {
            add_foreign_key_constraint(engine, &stmt.table, &stmt.qualifier, constraint)?;
        }
        AlterTableAction::AddNotNullConstraint {
            name,
            column,
            validated,
            no_inherit,
        } => {
            add_not_null_constraint(engine, &stmt.table, name, &column, validated, no_inherit)?;
        }
        AlterTableAction::ValidateConstraint { name } => {
            validate_and_mark_constraint(engine, &stmt.table, &name)?;
        }
        AlterTableAction::AlterConstraint {
            name,
            enforceability,
            deferrability,
            no_inherit,
        } => {
            alter_constraint(
                engine,
                &stmt.table,
                &name,
                enforceability,
                deferrability,
                no_inherit,
            )?;
        }
        AlterTableAction::DropConstraint {
            name,
            if_exists,
            cascade,
        } => {
            drop_constraint(engine, &stmt.table, &name, if_exists, cascade)?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: false,
        } => {
            drop_column_restrict(engine, &stmt.table, &name, if_exists)?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: true,
        } => {
            drop_column_cascade(engine, &stmt.table, &name, if_exists)?;
        }
        AlterTableAction::RenameColumn { from, to } => {
            if !engine
                .try_table_has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine
                .try_table_has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
            }
            engine
                .try_rename_column(&stmt.table, &from, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME COLUMN", e))?;
        }
        AlterTableAction::RenameTable { to } => {
            if engine
                .try_has_table(&to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine
                .try_rename_table(&stmt.table, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME", e))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::SetDefault { name, default } => {
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
            validate_default_expression(engine, &default)?;
            if !engine
                .set_column_default(&stmt.table, &name, Some(default))
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropDefault { name } => {
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
            if !engine
                .set_column_default(&stmt.table, &name, None)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::SetExpression { name, expression } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(current) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            let kind = current.kind;
            let generated = GeneratedColumn {
                kind,
                expression: Box::new(expression),
                function_dependencies: Vec::new(),
            };
            column.generated = Some(generated.clone());
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let generated = columns
                .iter()
                .find(|column| column.name == name)
                .and_then(|column| column.generated.clone())
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "generated column `{name}` disappeared during validation"
                    ))
                })?;
            engine
                .set_column_generated(&stmt.table, &name, Some(generated))
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            validate_and_rewrite_generated_rows(
                engine,
                &stmt.table,
                kind == GeneratedColumnKind::Stored,
            )?;
        }
        AlterTableAction::DropExpression { name } => {
            let columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(generated) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            if generated.kind == GeneratedColumnKind::Virtual {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE / DROP EXPRESSION is not supported for virtual generated column `{name}`"
                )));
            }
            engine
                .set_column_generated(&stmt.table, &name, None)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?;
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine
                .set_column_not_null(&stmt.table, &name, true)
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET NOT NULL", err))?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropNotNull { name } => {
            let (columns, _) = table_constraint_state(engine, &stmt.table)?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            if let Some(constraint_name) = column.not_null_name.as_deref() {
                drop_constraint(engine, &stmt.table, constraint_name, false, false)?;
            }
        }
        AlterTableAction::AlterColumnType { name, ty, using } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let candidate = candidate_columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            candidate.ty.clone_from(&ty);
            let target_generated_kind =
                candidate.generated.as_ref().map(|generated| generated.kind);
            if target_generated_kind.is_none() {
                let dependents = engine
                    .generated_columns_referencing_column(&stmt.table, &name)
                    .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
                if !dependents.is_empty() {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot alter type of column `{name}` because generated column(s) `{}` depend on it",
                        dependents.join("`, `")
                    )));
                }
            }
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let old_ty = engine
                .column_type(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let old_was_vector = matches!(&old_ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
            let new_is_vector = matches!(&ty, ColumnType::Vector(_) | ColumnType::Tensor(_));

            // Row rewrites maintain every currently registered vector index.
            // Detach a vector/tensor index before converting its values to a
            // scalar type, otherwise the first converted scalar is fed back
            // into the old vector index. The enclosing ALTER transaction
            // restores both catalog and physical index state if conversion
            // of any row subsequently fails.
            if old_was_vector && !new_is_vector {
                engine
                    .try_drop_vector_indexes_for_column(&stmt.table, &name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
            }
            if target_generated_kind.is_none() {
                rewrite_column_values_to_type(
                    engine,
                    &stmt.table,
                    &name,
                    &old_ty,
                    &ty,
                    using.as_ref(),
                )?;
            }
            engine
                .set_column_type(&stmt.table, &name, &ty)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            match ty {
                ColumnType::Text if target_generated_kind != Some(GeneratedColumnKind::Virtual) => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .try_rebuild_vector_index_for_column(&stmt.table, &name, dim)
                        .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
                }
                _ => {}
            }
            if let Some(kind) = target_generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstraintLocation {
    NotNull(usize),
    ColumnCheck(usize),
    ColumnForeignKey(usize),
    TableCheck(usize),
    TableForeignKey(usize),
    Key(usize),
}

fn constraint_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

fn table_constraint_state(
    engine: &Engine,
    table: &str,
) -> Result<
    (
        Vec<uqa_sql::ast::ColumnDef>,
        uqa_sql::ast::TableConstraintSet,
    ),
    SQLError,
> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint state", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let constraints = engine
        .try_declared_table_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint state", error))?;
    Ok((columns, constraints))
}

fn materialize_constraint_candidate(
    engine: &Engine,
    table: &str,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    let canonical = engine
        .try_resolve_table_name(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint naming", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let relation = crate::RelationIdentity::from_legacy_name(&canonical).map_err(|message| {
        SQLError::Internal(format!("constraint relation identity: {message}"))
    })?;
    crate::engine_table_storage::materialize_constraint_names(&relation, columns, constraints)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint naming", error))?;
    Ok(())
}

fn publish_constraint_state(
    engine: &Engine,
    table: &str,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    engine
        .replace_constraint_state(table, columns, constraints)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint catalog", error))
}

fn find_constraint(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
    name: &str,
) -> Option<ConstraintLocation> {
    columns
        .iter()
        .position(|column| column.not_null && column.not_null_name.as_deref() == Some(name))
        .map(ConstraintLocation::NotNull)
        .or_else(|| {
            columns
                .iter()
                .position(|column| {
                    column.check.is_some() && column.check_name.as_deref() == Some(name)
                })
                .map(ConstraintLocation::ColumnCheck)
        })
        .or_else(|| {
            columns
                .iter()
                .position(|column| {
                    column
                        .references
                        .as_ref()
                        .and_then(|reference| reference.name.as_deref())
                        == Some(name)
                })
                .map(ConstraintLocation::ColumnForeignKey)
        })
        .or_else(|| {
            constraints
                .checks
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::TableCheck)
        })
        .or_else(|| {
            constraints
                .foreign_keys
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::TableForeignKey)
        })
        .or_else(|| {
            constraints
                .key_constraints
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::Key)
        })
}

fn ensure_constraint_name_available(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
    name: Option<&str>,
    table: &str,
) -> Result<(), SQLError> {
    if let Some(name) = name.filter(|name| find_constraint(columns, constraints, name).is_some()) {
        return Err(constraint_error(
            "42710",
            format!("constraint \"{name}\" for relation \"{table}\" already exists"),
        ));
    }
    Ok(())
}

fn add_check_constraint(
    engine: &Engine,
    table: &str,
    mut constraint: uqa_sql::ast::TableCheck,
) -> Result<(), SQLError> {
    let should_validate = constraint.validated;
    constraint.validated = false;
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    ensure_constraint_name_available(&columns, &constraints, constraint.name.as_deref(), table)?;
    constraints.checks.push(constraint);
    materialize_constraint_candidate(engine, table, &mut columns, &mut constraints)?;
    let name = constraints
        .checks
        .last()
        .and_then(|constraint| constraint.name.clone())
        .ok_or_else(|| SQLError::Internal("new CHECK constraint has no name".into()))?;
    publish_constraint_state(engine, table, columns, constraints)?;
    if should_validate {
        validate_and_mark_constraint(engine, table, &name)?;
    }
    Ok(())
}

fn add_foreign_key_constraint(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    mut constraint: uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    validate_foreign_key_definition(engine, table, &mut constraint)?;
    let should_validate = constraint.validated;
    constraint.validated = false;
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    ensure_constraint_name_available(&columns, &constraints, constraint.name.as_deref(), table)?;
    constraints.foreign_keys.push(constraint);
    crate::sql::generated::prepare_generated_columns(
        engine,
        qualifier,
        &mut columns,
        &constraints.key_constraints,
        &constraints.foreign_keys,
    )?;
    materialize_constraint_candidate(engine, table, &mut columns, &mut constraints)?;
    let name = constraints
        .foreign_keys
        .last()
        .and_then(|constraint| constraint.name.clone())
        .ok_or_else(|| SQLError::Internal("new FOREIGN KEY constraint has no name".into()))?;
    publish_constraint_state(engine, table, columns, constraints)?;
    if should_validate {
        validate_and_mark_constraint(engine, table, &name)?;
    }
    Ok(())
}

fn add_not_null_constraint(
    engine: &Engine,
    table: &str,
    name: Option<String>,
    column: &str,
    validated: bool,
    no_inherit: bool,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    ensure_constraint_name_available(&columns, &constraints, name.as_deref(), table)?;
    let definition = columns
        .iter_mut()
        .find(|definition| definition.name == column)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{column}")))?;
    if definition.not_null {
        let existing = definition.not_null_name.as_deref().unwrap_or("<unnamed>");
        return Err(constraint_error(
            "55000",
            format!(
                "cannot create not-null constraint on column \"{column}\" of table \"{table}\": a not-null constraint named \"{existing}\" already exists for this column"
            ),
        ));
    }
    definition.not_null = true;
    definition.not_null_explicit = true;
    definition.not_null_name = name;
    definition.not_null_validated = false;
    definition.not_null_no_inherit = no_inherit;
    materialize_constraint_candidate(engine, table, &mut columns, &mut constraints)?;
    let name = columns
        .iter()
        .find(|definition| definition.name == column)
        .and_then(|definition| definition.not_null_name.clone())
        .ok_or_else(|| SQLError::Internal("new NOT NULL constraint has no name".into()))?;
    publish_constraint_state(engine, table, columns, constraints)?;
    if validated {
        validate_and_mark_constraint(engine, table, &name)?;
    }
    Ok(())
}

pub(super) fn validate_foreign_key_definition(
    engine: &Engine,
    table: &str,
    foreign_key: &mut uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("FOREIGN KEY local table", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    for column in &foreign_key.local_columns {
        if !columns.iter().any(|definition| definition.name == *column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    let referenced = engine
        .try_resolve_table_name(&foreign_key.ref_table)
        .map_err(|error| ddl_storage_error("FOREIGN KEY referenced table", error))?
        .ok_or_else(|| SQLError::UnknownTable(foreign_key.ref_table.clone()))?;
    let referenced_columns = engine
        .try_describe_table(&referenced)
        .map_err(|error| ddl_storage_error("FOREIGN KEY referenced columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(referenced.clone()))?;
    for column in &foreign_key.ref_columns {
        if !referenced_columns
            .iter()
            .any(|definition| definition.name == *column)
        {
            return Err(SQLError::UnknownColumn(format!("{referenced}.{column}")));
        }
    }
    let referenced_keys = engine
        .try_key_constraints(&referenced)
        .map_err(|error| ddl_storage_error("FOREIGN KEY referenced key", error))?;
    let has_unique_key = referenced_keys.iter().any(|key| {
        key.columns.len() == foreign_key.ref_columns.len()
            && foreign_key
                .ref_columns
                .iter()
                .all(|column| key.columns.contains(column))
    });
    if !has_unique_key {
        return Err(constraint_error(
            "42830",
            format!(
                "there is no unique constraint matching given keys for referenced table \"{referenced}\""
            ),
        ));
    }
    foreign_key.ref_table = referenced;
    Ok(())
}

fn validate_and_mark_constraint(engine: &Engine, table: &str, name: &str) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    let location = find_constraint(&columns, &constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        )
    })?;
    match location {
        ConstraintLocation::NotNull(index) => {
            if columns[index].not_null_validated {
                return Ok(());
            }
            validate_not_null_rows(engine, table, &columns[index].name)?;
            columns[index].not_null_validated = true;
        }
        ConstraintLocation::ColumnCheck(index) => {
            if columns[index].check_validated {
                return Ok(());
            }
            if !columns[index].check_enforced {
                return Err(constraint_error(
                    "55000",
                    "cannot validate NOT ENFORCED constraint",
                ));
            }
            let expression = columns[index]
                .check
                .clone()
                .ok_or_else(|| SQLError::Internal("column CHECK disappeared".into()))?;
            validate_check_rows(engine, table, name, &expression)?;
            columns[index].check_validated = true;
        }
        ConstraintLocation::TableCheck(index) => {
            if constraints.checks[index].validated {
                return Ok(());
            }
            if !constraints.checks[index].enforced {
                return Err(constraint_error(
                    "55000",
                    "cannot validate NOT ENFORCED constraint",
                ));
            }
            validate_check_rows(engine, table, name, &constraints.checks[index].expr)?;
            constraints.checks[index].validated = true;
        }
        ConstraintLocation::ColumnForeignKey(index) => {
            let reference = columns[index]
                .references
                .as_ref()
                .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?;
            if reference.validated {
                return Ok(());
            }
            if !reference.enforced {
                return Err(constraint_error(
                    "55000",
                    "cannot validate NOT ENFORCED constraint",
                ));
            }
            let foreign_key = column_foreign_key(&columns[index], reference);
            validate_foreign_key_rows(engine, table, name, &foreign_key)?;
            columns[index]
                .references
                .as_mut()
                .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?
                .validated = true;
        }
        ConstraintLocation::TableForeignKey(index) => {
            if constraints.foreign_keys[index].validated {
                return Ok(());
            }
            if !constraints.foreign_keys[index].enforced {
                return Err(constraint_error(
                    "55000",
                    "cannot validate NOT ENFORCED constraint",
                ));
            }
            validate_foreign_key_rows(engine, table, name, &constraints.foreign_keys[index])?;
            constraints.foreign_keys[index].validated = true;
        }
        ConstraintLocation::Key(_) => {
            return Err(constraint_error(
                "42809",
                format!(
                    "constraint \"{name}\" of relation \"{table}\" is not a foreign key, check, or not-null constraint"
                ),
            ));
        }
    }
    publish_constraint_state(engine, table, columns, constraints)
}

fn column_foreign_key(
    column: &uqa_sql::ast::ColumnDef,
    reference: &uqa_sql::ast::ForeignKeyRef,
) -> uqa_sql::ast::ForeignKey {
    uqa_sql::ast::ForeignKey {
        name: reference.name.clone(),
        local_columns: vec![column.name.clone()],
        ref_table: reference.table.clone(),
        ref_columns: vec![reference.column.clone()],
        on_update: reference.on_update,
        on_delete: reference.on_delete,
        on_delete_set_columns: Vec::new(),
        match_type: reference.match_type,
        enforced: reference.enforced,
        validated: reference.validated,
        deferrable: reference.deferrable,
        initially_deferred: reference.initially_deferred,
    }
}

fn validate_not_null_rows(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        if matches!(document.get(column), None | Some(Value::Null)) {
            return Err(constraint_error(
                "23502",
                format!("column \"{column}\" of relation \"{table}\" contains null values"),
            ));
        }
    }
    Ok(())
}

fn validate_check_rows(
    engine: &Engine,
    table: &str,
    name: &str,
    expression: &uqa_sql::ast::Expr,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("VALIDATE CHECK", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = uqa_execution::RowSchema::with_types(
        definitions
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    for doc_id in engine.table_doc_ids(table)? {
        let Some(mut document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        crate::engine_generated::materialize_virtual_generated_columns(
            &definitions,
            &mut document,
        )?;
        let value = crate::sql::scalar::eval_lowered_expression_with_schema(
            engine,
            expression,
            &document,
            &schema,
            &[],
        )?;
        if !matches!(value, Value::Null) && !uqa_sql::expr::truthy(&value) {
            return Err(constraint_error(
                "23514",
                format!(
                    "check constraint \"{name}\" of relation \"{table}\" is violated by some row"
                ),
            ));
        }
    }
    Ok(())
}

fn validate_foreign_key_rows(
    engine: &Engine,
    table: &str,
    name: &str,
    foreign_key: &uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let Some(values) = crate::sql::dml::foreign_key_lookup_values(foreign_key, &document)?
        else {
            continue;
        };
        if engine
            .find_conflict(&foreign_key.ref_table, &foreign_key.ref_columns, &values)?
            .is_none()
        {
            return Err(foreign_key_violation(table, name));
        }
    }
    Ok(())
}

fn foreign_key_violation(table: &str, name: &str) -> SQLError {
    constraint_error(
        "23503",
        format!("insert or update on table \"{table}\" violates foreign key constraint \"{name}\""),
    )
}

fn alter_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
    enforceability: Option<bool>,
    deferrability: Option<(bool, bool)>,
    no_inherit: Option<bool>,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    let location = find_constraint(&columns, &constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        )
    })?;
    let is_foreign_key = matches!(
        location,
        ConstraintLocation::ColumnForeignKey(_) | ConstraintLocation::TableForeignKey(_)
    );
    let is_not_null = matches!(location, ConstraintLocation::NotNull(_));
    if enforceability.is_some() && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!("cannot alter enforceability of constraint \"{name}\" of relation \"{table}\""),
        ));
    }
    if deferrability.is_some() && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!(
                "constraint \"{name}\" of relation \"{table}\" is not a foreign key constraint"
            ),
        ));
    }
    if no_inherit.is_some() && !is_not_null {
        return Err(constraint_error(
            "42809",
            format!("constraint \"{name}\" of relation \"{table}\" is not a not-null constraint"),
        ));
    }
    let mut validate_after_publish = false;
    match location {
        ConstraintLocation::NotNull(index) => {
            if let Some(no_inherit) = no_inherit {
                columns[index].not_null_no_inherit = no_inherit;
            }
        }
        ConstraintLocation::ColumnForeignKey(index) => {
            let foreign_key = columns[index]
                .references
                .as_mut()
                .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?;
            if let Some(enforced) = enforceability {
                if !enforced {
                    foreign_key.enforced = false;
                    foreign_key.validated = false;
                } else if !foreign_key.enforced {
                    foreign_key.enforced = true;
                    foreign_key.validated = false;
                    validate_after_publish = true;
                }
            }
            if let Some((deferrable, initially_deferred)) = deferrability {
                foreign_key.deferrable = deferrable;
                foreign_key.initially_deferred = initially_deferred;
            }
        }
        ConstraintLocation::TableForeignKey(index) => {
            let foreign_key = &mut constraints.foreign_keys[index];
            if let Some(enforced) = enforceability {
                if !enforced {
                    foreign_key.enforced = false;
                    foreign_key.validated = false;
                } else if !foreign_key.enforced {
                    foreign_key.enforced = true;
                    foreign_key.validated = false;
                    validate_after_publish = true;
                }
            }
            if let Some((deferrable, initially_deferred)) = deferrability {
                foreign_key.deferrable = deferrable;
                foreign_key.initially_deferred = initially_deferred;
            }
        }
        ConstraintLocation::ColumnCheck(_)
        | ConstraintLocation::TableCheck(_)
        | ConstraintLocation::Key(_) => {}
    }
    publish_constraint_state(engine, table, columns, constraints)?;
    if validate_after_publish {
        validate_and_mark_constraint(engine, table, name)?;
    }
    Ok(())
}

fn reject_default_change_on_generated_column(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    if crate::sql::generated::generated_column_kind(engine, table, column)?.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` of relation `{table}` is a generated column; use SET EXPRESSION or DROP EXPRESSION"
        )));
    }
    Ok(())
}

fn validate_and_rewrite_generated_rows(
    engine: &Engine,
    table: &str,
    rewrite_physical_rows: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
    let mut rows = Vec::with_capacity(doc_ids.len());
    for doc_id in &doc_ids {
        let Some(mut document) = engine.get_document(table, *doc_id)? else {
            continue;
        };
        crate::sql::generated::refresh_stored_generated_columns(engine, table, &mut document)?;
        rows.push((*doc_id, document));
    }
    validate_key_constraint_rows(engine, table, &rows)?;
    if rewrite_physical_rows {
        let mut replacements = Vec::with_capacity(rows.len());
        let mut remaps_primary_key = false;
        for (old_doc_id, document) in rows {
            let new_doc_id = crate::sql::dml::integer_primary_key_doc_id(engine, table, &document)?
                .unwrap_or(old_doc_id);
            remaps_primary_key |= new_doc_id != old_doc_id;
            replacements.push((old_doc_id, new_doc_id, document));
        }
        if remaps_primary_key {
            for (old_doc_id, _, _) in &replacements {
                engine.delete_document(table, *old_doc_id)?;
            }
        }
        for (old_doc_id, new_doc_id, document) in replacements {
            let vectors = crate::sql::dml::document_vectors(engine, table, &document)?;
            engine.add_prepared_document_with_vector_values(
                table,
                if remaps_primary_key {
                    new_doc_id
                } else {
                    old_doc_id
                },
                document,
                vectors,
                remaps_primary_key,
            )?;
            if remaps_primary_key {
                engine
                    .advance_next_id(table, new_doc_id)
                    .map_err(|error| ddl_storage_error("generated primary key rewrite", error))?;
            }
        }
    }
    validate_all_table_rows(engine)
}

fn validate_key_constraint_rows(
    engine: &Engine,
    table: &str,
    rows: &[(uqa_core::DocId, Document)],
) -> Result<(), SQLError> {
    for constraint in engine
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        let mut seen = std::collections::BTreeSet::new();
        for (_, document) in rows {
            let values = constraint
                .columns
                .iter()
                .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>();
            let contains_null = values.iter().any(|value| matches!(value, Value::Null));
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey && contains_null
            {
                return Err(SQLError::TypeMismatch(format!(
                    "PRIMARY KEY constraint contains NULL values on table `{table}`"
                )));
            }
            if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
                && contains_null
                && !constraint.nulls_not_distinct
            {
                continue;
            }
            if !seen.insert(values) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint would be violated by generated values on table `{table}`",
                    match constraint.kind {
                        uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
                        uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
                    }
                )));
            }
        }
    }
    Ok(())
}

fn validate_all_table_rows(engine: &Engine) -> Result<(), SQLError> {
    for table in engine
        .table_names()
        .map_err(|error| ddl_storage_error("generated-column validation", error))?
    {
        for doc_id in engine.table_doc_ids(&table)? {
            let Some(document) = engine.get_document(&table, doc_id)? else {
                continue;
            };
            crate::sql::dml::validate_document_constraints(
                engine,
                &table,
                &document,
                &[],
                Some(doc_id),
            )?;
        }
    }
    Ok(())
}

fn ensure_column_exists(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
    if engine
        .try_table_has_column(table, column)
        .map_err(|err| ddl_storage_error("ALTER COLUMN", err))?
    {
        Ok(())
    } else {
        Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` does not exist"
        )))
    }
}

fn ensure_existing_values_not_null(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let mut null_rows = 0usize;
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        if matches!(doc.get(column), None | Some(Value::Null)) {
            null_rows += 1;
        }
    }
    if null_rows > 0 {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` contains NULL values"
        )));
    }
    Ok(())
}

fn validate_added_key_constraint(
    engine: &Engine,
    table: &str,
    constraint: &uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let column_names: std::collections::BTreeSet<&str> =
        columns.iter().map(|column| column.name.as_str()).collect();
    for column in &constraint.columns {
        if !column_names.contains(column.as_str()) {
            return Err(SQLError::TypeMismatch(format!(
                "ALTER TABLE ADD CONSTRAINT references unknown column `{column}`"
            )));
        }
    }

    let existing_keys = engine
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    if let Some(name) = constraint.name.as_deref() {
        let check_name_exists = engine
            .try_check_constraint_definitions(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let foreign_name_exists = engine
            .try_foreign_keys(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let key_name_exists = existing_keys
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        if check_name_exists || foreign_name_exists || key_name_exists {
            return Err(SQLError::TypeMismatch(format!(
                "constraint `{name}` already exists on table `{table}`"
            )));
        }
    }
    if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey
        && existing_keys
            .iter()
            .any(|existing| existing.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey)
    {
        return Err(SQLError::TypeMismatch(format!(
            "multiple PRIMARY KEY constraints are not allowed on table `{table}`"
        )));
    }

    let mut seen = std::collections::BTreeSet::<Vec<Value>>::new();
    for doc_id in engine.table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let values: Vec<Value> = constraint
            .columns
            .iter()
            .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        let contains_null = values.iter().any(|value| matches!(value, Value::Null));
        if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey && contains_null {
            return Err(SQLError::TypeMismatch(format!(
                "PRIMARY KEY constraint contains NULL values on table `{table}`"
            )));
        }
        if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
            && contains_null
            && !constraint.nulls_not_distinct
        {
            continue;
        }
        if !seen.insert(values) {
            return Err(SQLError::TypeMismatch(format!(
                "{} constraint would be violated by duplicate values on table `{table}`",
                match constraint.kind {
                    uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
                    uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
                }
            )));
        }
    }
    Ok(())
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; this function preserves that semantic by
/// sweeping the document store.
fn backfill_added_column(
    engine: &Engine,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table)?;
    if doc_ids.is_empty() {
        return Ok(());
    }
    let default_value = if let Some(expr) = default_expr {
        eval_lowered_expression(engine, expr, None, &[])?
    } else if not_null {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ADD COLUMN `{column}` is NOT NULL but no DEFAULT supplied; \
             {} existing row(s) would violate the constraint",
            doc_ids.len()
        )));
    } else {
        Value::Null
    };
    let default_value = coerce_to_column_type(engine, table, column, default_value)?;
    let vector_value: Option<Vec<Vec<f32>>> = match engine
        .column_type(table, column)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
    {
        Some(ty) if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) => {
            Some(index_vectors_for_type(&default_value, &ty)?)
        }
        Some(_) | None => None,
    };
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

// DDL
