//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 ALTER TABLE constraint lifecycle.

use super::foreign_key::{
    column_foreign_key, validate_foreign_key_definition, validate_foreign_key_rows,
};
use super::{
    ddl_storage_error, resolve_foreign_key_parent, validate_temporal_foreign_key, ColumnType,
    Engine, ForeignKey, SQLError,
};

pub(super) fn validate_altered_constraint_column_types(
    engine: &Engine,
    table: &str,
    candidate_columns: &[uqa_sql::ast::ColumnDef],
    key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    for constraint in key_constraints
        .iter()
        .filter(|constraint| constraint.without_overlaps)
    {
        let Some(period_column) = constraint.columns.last() else {
            return Err(SQLError::Internal(
                "WITHOUT OVERLAPS constraint has no period column".into(),
            ));
        };
        let period_type = candidate_columns
            .iter()
            .find(|column| column.name == *period_column)
            .map(|column| &column.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
        if !matches!(
            period_type,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "column \"{period_column}\" in WITHOUT OVERLAPS is not a range or multirange type"
                ),
            });
        }
    }

    for foreign_key in foreign_keys.iter().filter(|foreign_key| foreign_key.period) {
        let (parent_name, parent_columns, parent_keys) =
            resolve_foreign_key_parent(engine, &foreign_key.ref_table)?;
        let parent_columns = if parent_name == table {
            candidate_columns
        } else {
            parent_columns.as_slice()
        };
        validate_temporal_foreign_key(
            table,
            candidate_columns,
            &parent_name,
            parent_columns,
            &parent_keys,
            foreign_key,
        )?;
    }

    for (child_table, foreign_key) in engine
        .try_referrers_to(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .into_iter()
        .filter(|(_, foreign_key)| foreign_key.period)
    {
        let child_columns = if child_table == table {
            candidate_columns.to_vec()
        } else {
            engine
                .try_describe_table(&child_table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(child_table.clone()))?
        };
        validate_temporal_foreign_key(
            &child_table,
            &child_columns,
            table,
            candidate_columns,
            key_constraints,
            &foreign_key,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConstraintLocation {
    NotNull(usize),
    ColumnCheck(usize),
    ColumnForeignKey(usize),
    TableCheck(usize),
    TableForeignKey(usize),
    Key(usize),
}

pub(super) fn constraint_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

pub(super) fn table_constraint_state(
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
    super::super::constraint_indexes::name_constraint_indexes(
        engine,
        &canonical,
        &mut constraints.key_constraints,
    )?;
    crate::table_storage::materialize_constraint_metadata(&relation, columns, constraints)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint naming", error))?;
    Ok(())
}

pub(super) fn publish_constraint_state(
    engine: &Engine,
    table: &str,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    engine
        .replace_constraint_state(table, columns, constraints)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint catalog", error))?;
    engine.prune_constraint_modes()
}

pub(super) fn find_constraint(
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

pub(super) fn ensure_constraint_name_available(
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

pub(super) fn add_check_constraint(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    mut constraint: uqa_sql::ast::TableCheck,
) -> Result<(), SQLError> {
    let should_validate = constraint.validated;
    if super::checks::merge_added_check(engine, table, constraint.clone())? {
        return Ok(());
    }
    constraint.validated = false;
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    super::super::constraint_validation::validate_check_expression(
        engine,
        table,
        qualifier,
        &columns,
        &mut constraint.expr,
    )?;
    crate::sql::reject_stored_regrole_constants(engine, &constraint.expr, None)?;
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

pub(super) fn add_foreign_key_constraint(
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

pub(super) fn ensure_not_null_inheritable(
    table: &str,
    column: &uqa_sql::ast::ColumnDef,
    sqlstate: &str,
) -> Result<(), SQLError> {
    if column.not_null_no_inherit {
        let relation = crate::RelationIdentity::from_legacy_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve NOT NULL relation: {error}")))?;
        let name = column.not_null_name.as_deref().unwrap_or("<unnamed>");
        return Err(constraint_error(
            sqlstate,
            format!(
            "cannot change NO INHERIT status of NOT NULL constraint \"{name}\" on relation \"{}\"",
            relation.name,
        ),
        ));
    }
    Ok(())
}

pub(super) fn set_not_null_constraint(
    engine: &Engine,
    table: &str,
    column: &str,
    recurse: bool,
    is_local: bool,
    inherited_name: Option<String>,
) -> Result<(), SQLError> {
    let (mut columns, constraints) = table_constraint_state(engine, table)?;
    let relation = crate::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve NOT NULL relation: {error}")))?;
    if super::super::POSTGRES_SYSTEM_COLUMNS.contains(&column) {
        return Err(constraint_error(
            "0A000",
            format!("cannot alter system column \"{column}\""),
        ));
    }
    let definition = columns
        .iter_mut()
        .find(|definition| definition.name == column)
        .ok_or_else(|| {
            constraint_error(
                "42703",
                format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    relation.name,
                ),
            )
        })?;
    if definition.not_null {
        if recurse {
            ensure_not_null_inheritable(table, definition, "0A000")?;
        }
        let name = definition
            .not_null_name
            .clone()
            .ok_or_else(|| SQLError::Internal("existing NOT NULL constraint has no name".into()))?;
        let became_local = is_local && !definition.not_null_is_local;
        if is_local && (!definition.not_null_explicit || became_local) {
            definition.not_null_explicit = true;
            definition.not_null_is_local = true;
            publish_constraint_state(engine, table, columns, constraints)?;
        }
        if became_local {
            return Ok(());
        }
        return validate_and_mark_constraint(engine, table, &name);
    }
    let no_inherit = !recurse && !engine.direct_hierarchy_children(table)?.is_empty();
    if no_inherit && constraints.hierarchy.partition_spec.is_some() {
        return Err(constraint_error(
            "42P16",
            "constraint must be added to child tables too",
        ));
    }
    add_not_null_constraint(
        engine,
        table,
        inherited_name,
        column,
        true,
        no_inherit,
        is_local,
    )
}

pub(super) fn add_not_null_constraint(
    engine: &Engine,
    table: &str,
    name: Option<String>,
    column: &str,
    validated: bool,
    no_inherit: bool,
    is_local: bool,
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
    definition.not_null_is_local = is_local;
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

pub(super) fn validate_and_mark_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
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

pub(super) fn validate_not_null_rows(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    uqa_execution::schema::validation::validate_not_null_rows(engine, table, column)
}
fn validate_check_rows(
    engine: &Engine,
    table: &str,
    name: &str,
    expression: &uqa_sql::ast::Expr,
) -> Result<(), SQLError> {
    uqa_execution::schema::validation::validate_check_rows(
        &uqa_execution::schema::validation::CheckValidationContext {
            columns: engine,
            reads: engine,
            expressions: engine,
        },
        table,
        name,
        expression,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub(super) fn alter_constraint(
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
    let recreated_foreign_key = if enforceability == Some(true) {
        match location {
            ConstraintLocation::ColumnForeignKey(index) => columns[index]
                .references
                .as_ref()
                .filter(|foreign_key| !foreign_key.enforced)
                .map(|foreign_key| column_foreign_key(&columns[index], foreign_key)),
            ConstraintLocation::TableForeignKey(index) => constraints
                .foreign_keys
                .get(index)
                .filter(|foreign_key| !foreign_key.enforced)
                .cloned(),
            ConstraintLocation::NotNull(_)
            | ConstraintLocation::ColumnCheck(_)
            | ConstraintLocation::TableCheck(_)
            | ConstraintLocation::Key(_) => None,
        }
        .map(|foreign_key| engine.foreign_key_constraint_identity(table, &foreign_key))
        .transpose()?
    } else {
        None
    };
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
    if let Some(identity) = &recreated_foreign_key {
        engine.forget_named_constraint_mode(identity);
    }
    Ok(())
}
