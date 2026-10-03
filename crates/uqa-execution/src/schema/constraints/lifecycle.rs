//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schedule constraint creation, validation, and enforcement changes.
use super::{
    checks, constraint_error, ddl_storage_error, ensure_not_null_inheritable, find_constraint,
    foreign_key_constraint_identity, materialize_constraint_candidate, publish_constraint_state,
    table_constraint_state, validate_check_expression, ConstraintAlterContext, ConstraintLocation,
    SQLError,
};
use uqa_sql::schema::foreign_keys::column_foreign_key;

pub fn add_check_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    qualifier: &str,
    mut constraint: uqa_sql::ast::TableCheck,
) -> Result<(), SQLError> {
    let should_validate = constraint.validated;
    if checks::merge_added_check(context, table, constraint.clone())? {
        return Ok(());
    }
    constraint.validated = false;
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    validate_check_expression(context, table, qualifier, &columns, &mut constraint.expr)?;
    uqa_sql::catalog::regrole_dependencies::reject_stored_regrole_constants(
        context.publication.bindings.schema,
        &constraint.expr,
        None,
    )?;
    context
        .publication
        .constraint_names()
        .ensure_available(table, constraint.name.as_deref())?;
    constraints.checks.push(constraint);
    materialize_constraint_candidate(context, table, &mut columns, &mut constraints)?;
    let name = constraints
        .checks
        .last()
        .and_then(|constraint| constraint.name.clone())
        .ok_or_else(|| SQLError::Internal("new CHECK constraint has no name".into()))?;
    publish_constraint_state(context, table, columns, constraints)?;
    if should_validate {
        validate_and_mark_constraint(context, table, &name)?;
    }
    Ok(())
}

pub fn add_foreign_key_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    qualifier: &str,
    mut constraint: uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    uqa_sql::schema::foreign_keys::validate_foreign_key_definition(
        &context.foreign_keys,
        table,
        &mut constraint,
    )?;
    let should_validate = constraint.validated;
    constraint.validated = false;
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    context
        .publication
        .constraint_names()
        .ensure_available(table, constraint.name.as_deref())?;
    constraints.foreign_keys.push(constraint);
    uqa_sql::schema::generated::prepare_generated_columns(
        context.publication.bindings.schema,
        qualifier,
        &mut columns,
        &constraints.key_constraints,
        &constraints.foreign_keys,
    )?;
    materialize_constraint_candidate(context, table, &mut columns, &mut constraints)?;
    let name = constraints
        .foreign_keys
        .last()
        .and_then(|constraint| constraint.name.clone())
        .ok_or_else(|| SQLError::Internal("new FOREIGN KEY constraint has no name".into()))?;
    publish_constraint_state(context, table, columns, constraints)?;
    if is_partitioned(context, table)? {
        let (_, constraints) = table_constraint_state(context, table)?;
        let key = constraints
            .foreign_keys
            .iter()
            .find(|constraint| constraint.name.as_deref() == Some(name.as_str()))
            .ok_or_else(|| SQLError::Internal("new FOREIGN KEY constraint disappeared".into()))?;
        super::partition_foreign_keys::inherit_partition_foreign_key(context, table, key)?;
    }
    if should_validate {
        validate_and_mark_constraint(context, table, &name)?;
    }
    Ok(())
}

fn is_partitioned(context: &ConstraintAlterContext<'_>, table: &str) -> Result<bool, SQLError> {
    Ok(context
        .rows
        .partitions
        .catalog
        .try_table_hierarchy(table)
        .map_err(SQLError::Internal)?
        .partition_spec
        .is_some())
}

pub fn set_not_null_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    column: &str,
    recurse: bool,
    is_local: bool,
    inherited_name: Option<String>,
) -> Result<(), SQLError> {
    let (mut columns, constraints) = table_constraint_state(context, table)?;
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve NOT NULL relation: {error}")))?;
    if uqa_sql::schema::columns::POSTGRES_SYSTEM_COLUMNS.contains(&column) {
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
            publish_constraint_state(context, table, columns, constraints)?;
        }
        if became_local {
            return Ok(());
        }
        return validate_and_mark_constraint(context, table, &name);
    }
    let no_inherit = !recurse
        && !context
            .rows
            .partitions
            .catalog
            .direct_hierarchy_children(table)?
            .is_empty();
    if no_inherit && constraints.hierarchy.partition_spec.is_some() {
        return Err(constraint_error(
            "42P16",
            "constraint must be added to child tables too",
        ));
    }
    add_not_null_constraint(
        context,
        table,
        inherited_name,
        column,
        true,
        no_inherit,
        is_local,
    )
}

pub fn add_not_null_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: Option<String>,
    column: &str,
    validated: bool,
    no_inherit: bool,
    is_local: bool,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    context
        .publication
        .constraint_names()
        .ensure_available(table, name.as_deref())?;
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
    definition.not_null_identity = None;
    definition.not_null_validated = false;
    definition.not_null_no_inherit = no_inherit;
    definition.not_null_is_local = is_local;
    materialize_constraint_candidate(context, table, &mut columns, &mut constraints)?;
    let name = columns
        .iter()
        .find(|definition| definition.name == column)
        .and_then(|definition| definition.not_null_name.clone())
        .ok_or_else(|| SQLError::Internal("new NOT NULL constraint has no name".into()))?;
    publish_constraint_state(context, table, columns, constraints)?;
    if validated {
        validate_and_mark_constraint(context, table, &name)?;
    }
    Ok(())
}

pub fn validate_and_mark_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    let location = find_constraint(&columns, &constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        )
    })?;
    match location {
        ConstraintLocation::ReferencedPartition(foreign_key, index) => {
            if !validate_referenced_partition(
                context,
                table,
                name,
                &mut columns,
                &mut constraints,
                foreign_key,
                index,
            )? {
                return Ok(());
            }
        }
        ConstraintLocation::NotNull(index) => {
            if columns[index].not_null_validated {
                return Ok(());
            }
            validate_not_null_rows(context, table, &columns[index].name)?;
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
            validate_check_rows(context, table, name, &expression)?;
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
            validate_check_rows(context, table, name, &constraints.checks[index].expr)?;
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
            validate_foreign_key(
                context,
                table,
                name,
                &column_foreign_key(&columns[index], reference),
            )?;
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
            validate_foreign_key(context, table, name, &constraints.foreign_keys[index])?;
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
    publish_constraint_state(context, table, columns, constraints)
}

/// Validate a constraint a foreign key derives on a referenced partition, and the constraints deriving from it, which `PostgreSQL` marks valid without validating the foreign key. A referencing table that is not partitioned has its rows validated against that partition alone, as `QueueFKConstraintValidation` validates them against the derived constraint's referenced relation. Reports whether the constraint was not yet valid.
fn validate_referenced_partition(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
    foreign_key: uqa_sql::schema::constraint_changes::ForeignKeyLocation,
    index: usize,
) -> Result<bool, SQLError> {
    let holder = match foreign_key {
        uqa_sql::schema::constraint_changes::ForeignKeyLocation::Column(position) => {
            uqa_sql::schema::inheritance::foreign_keys::DeclaredForeignKey::Column(position)
        }
        uqa_sql::schema::constraint_changes::ForeignKeyLocation::Table(position) => {
            uqa_sql::schema::inheritance::foreign_keys::DeclaredForeignKey::Table(position)
        }
    };
    let mut scoped = holder
        .foreign_key(columns, constraints)
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY disappeared".into()))?;
    let derived = scoped
        .referenced_partitions
        .get(index)
        .cloned()
        .ok_or_else(|| SQLError::Internal("derived constraint disappeared".into()))?;
    if derived.validated {
        return Ok(false);
    }
    if !scoped.enforced {
        return Err(constraint_error(
            "55000",
            "cannot validate NOT ENFORCED constraint",
        ));
    }
    if !is_partitioned(context, table)? {
        scoped.ref_table = context
            .lock_catalog
            .table_name(derived.partition)
            .ok_or_else(|| SQLError::Internal("referenced partition disappeared".into()))?;
        crate::schema::validation::validate_foreign_key_rows(context.rows, table, name, &scoped)?;
    }
    let entries = foreign_key
        .derived_mut(columns, constraints)
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY disappeared".into()))?;
    let mut validated = std::collections::BTreeSet::from([derived.partition]);
    for entry in entries.iter_mut() {
        if entry.partition == derived.partition
            || entry
                .parent
                .is_some_and(|parent| validated.contains(&parent))
        {
            entry.validated = true;
            validated.insert(entry.partition);
        }
    }
    Ok(true)
}

/// Validate the rows of an enforced foreign key: the table's own rows, and on a partitioned table the rows of each leaf partition through its copy.
fn validate_foreign_key(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    foreign_key: &uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    if !foreign_key.enforced {
        return Err(constraint_error(
            "55000",
            "cannot validate NOT ENFORCED constraint",
        ));
    }
    crate::schema::validation::validate_foreign_key_rows(context.rows, table, name, foreign_key)?;
    if is_partitioned(context, table)? {
        super::partition_foreign_keys::validate_partition_foreign_keys(
            context,
            table,
            foreign_key.object_id.ok_or_else(|| {
                SQLError::Internal("materialized foreign key has no object identity".into())
            })?,
        )?;
    }
    Ok(())
}

pub fn validate_not_null_rows(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    crate::schema::validation::validate_not_null_rows(context.rows.reads, table, column)
}

fn validate_check_rows(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    expression: &uqa_sql::ast::Expr,
) -> Result<(), SQLError> {
    crate::schema::validation::validate_check_rows(
        &crate::schema::validation::CheckValidationContext {
            columns: context.foreign_keys.columns,
            reads: context.rows.reads,
            expressions: context.rows.partitions.expressions,
        },
        table,
        name,
        expression,
    )
}

/// `ALTER CONSTRAINT`, which a partitioned table's foreign key applies to the copies on its partitions as well, and which a partition's copy refuses, as `PostgreSQL` does. `ONLY` cannot alter a constraint of a partitioned table.
pub fn alter_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    mut options: uqa_sql::schema::constraint_changes::ConstraintAlterOptions,
    recurse: bool,
) -> Result<(), SQLError> {
    let partitioned = is_partitioned(context, table)?;
    if !recurse && partitioned {
        return Err(SQLError::Diagnostic {
            sqlstate: "42P16".into(),
            message: "constraint must be altered in child tables too".into(),
            detail: None,
            hint: Some("Do not specify the ONLY keyword.".into()),
        });
    }
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    let family = foreign_key_family(&columns, &constraints, name);
    if let Some(family) = family {
        options.ancestor =
            super::partition_foreign_keys::foreign_key_ancestor(context, table, family)?;
    }
    let (enforceability, deferrability) = (options.enforceability, options.deferrability);
    let effects = uqa_sql::schema::constraint_changes::apply_constraint_alteration(
        table,
        name,
        &mut columns,
        &mut constraints,
        options,
    )?;
    let recreated_foreign_key = effects
        .recreated_foreign_key
        .as_ref()
        .map(|foreign_key| foreign_key_constraint_identity(context, table, foreign_key))
        .transpose()?;
    publish_constraint_state(context, table, columns, constraints)?;
    if let Some(family) = family.filter(|_| partitioned) {
        super::partition_foreign_keys::alter_partition_foreign_keys(
            context,
            table,
            family,
            enforceability,
            deferrability,
        )?;
    }
    if effects.validate_after_publish {
        validate_and_mark_constraint(context, table, name)?;
    }
    if let Some(identity) = &recreated_foreign_key {
        context.modes.forget(identity);
    }
    Ok(())
}

/// The object identity of the foreign key named `name`, which its partition copies share.
fn foreign_key_family(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
    name: &str,
) -> Option<[u8; 16]> {
    match find_constraint(columns, constraints, name)? {
        ConstraintLocation::ColumnForeignKey(index) => {
            columns[index].references.as_ref()?.object_id
        }
        ConstraintLocation::TableForeignKey(index) => constraints.foreign_keys[index].object_id,
        _ => None,
    }
}

pub fn add_key_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    qualifier: &str,
    mut constraint: uqa_sql::ast::TableKeyConstraint,
) -> Result<(), SQLError> {
    let mut columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    context
        .publication
        .constraint_names()
        .ensure_available(table, constraint.name.as_deref())?;
    uqa_sql::schema::indexes::names::name_constraint_indexes(
        context.names,
        table,
        std::slice::from_mut(&mut constraint),
    )?;
    let mut key_constraints = context
        .catalog
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    key_constraints.push(constraint.clone());
    let foreign_keys = context
        .catalog
        .try_foreign_keys(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    uqa_sql::schema::generated::prepare_generated_columns(
        context.publication.bindings.schema,
        qualifier,
        &mut columns,
        &key_constraints,
        &foreign_keys,
    )?;
    crate::schema::keys::validate_added_key_constraint(
        &crate::schema::keys::KeyValidationContext {
            catalog: context.catalog,
            constraints: context.rows,
        },
        table,
        &constraint,
    )?;
    context
        .writes
        .with_schema_write(Box::new(|publication| {
            crate::schema::publication::keys::append_key_constraint(publication, table, &constraint)
        }))
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
    Ok(())
}
