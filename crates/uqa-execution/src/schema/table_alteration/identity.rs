//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute the `ALTER COLUMN` identity actions as `PostgreSQL`'s `ATExecAddIdentity`, `ATExecSetIdentity` and `ATExecDropIdentity` do. A partitioned table's partitions share its identity sequence and follow each change; an inheritance child never takes identity.

use super::{ddl_storage_error, TableAlterContext};
use crate::schema::sequences::{
    alteration::SequenceDefinitionContext, dispatch::SequenceCommandCatalog,
};
use uqa_core::RelationIdentity;
use uqa_sql::ast::{
    AlterSequence, AutoIncrement, AutoIncrementKind, ColumnDef, DeferredSQLError,
    IdentitySequenceDeclaration, SequenceBound, SequenceDeclaration, SequenceRestart,
};
use uqa_sql::SQLError;

/// The sequence catalog the identity actions change.
pub struct IdentityAlterContext<'a> {
    pub definitions: SequenceDefinitionContext<'a>,
    pub catalog: &'a dyn SequenceCommandCatalog,
}

/// Where an action runs in a partition hierarchy.
#[derive(Clone, Copy)]
pub(super) struct IdentityTarget<'a> {
    pub table: &'a str,
    /// `ALTER TABLE` without `ONLY`.
    pub recurse: bool,
    /// A partition the action reached from its partitioned parent.
    pub recursing: bool,
}

/// The table's local name, which `PostgreSQL` diagnostics use.
fn local_name(table: &str) -> Result<String, SQLError> {
    RelationIdentity::from_legacy_name(table)
        .map(|relation| relation.name)
        .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE target: {error}")))
}

fn column_error(
    sqlstate: &str,
    table: &str,
    column: &str,
    detail: &str,
) -> Result<SQLError, SQLError> {
    Ok(SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!(
            "column \"{column}\" of relation \"{}\" {detail}",
            local_name(table)?
        ),
    })
}

fn only_partitioned_error(message: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P16".into(),
        message: message.into(),
        detail: None,
        hint: Some("Do not specify the ONLY keyword.".into()),
    }
}

fn partition_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: message.into(),
    }
}

/// The column `name` of `table`. `PostgreSQL` reports a system column as one it cannot alter (`0A000`) and any other missing name with `42703`.
fn column<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    name: &str,
) -> Result<ColumnDef, SQLError> {
    context
        .hierarchy
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN identity", error))?
        .unwrap_or_default()
        .into_iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| uqa_sql::schema::columns::missing_altered_column(table, name))
}

/// Whether a column draws its values as an identity column. A column an earlier version gave a table counter counts as one.
fn is_identity(column: &ColumnDef) -> bool {
    column
        .auto_increment
        .as_ref()
        .is_some_and(|provenance| provenance.is_identity() || provenance.is_legacy())
}

/// Whether `table` is partitioned, and whether it is a partition.
fn partitioning<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
) -> Result<(bool, bool), SQLError> {
    let hierarchy = context
        .hierarchy
        .partitions
        .catalog
        .try_table_hierarchy(table)
        .map_err(SQLError::Internal)?;
    Ok((
        hierarchy.partition_spec.is_some(),
        hierarchy.partition_bound.is_some(),
    ))
}

/// The partitions an action on the partitioned `table` reaches.
fn partitions<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
) -> Result<Vec<String>, SQLError> {
    context
        .hierarchy
        .partitions
        .catalog
        .direct_hierarchy_children(table)
}

/// `ADD GENERATED ... AS IDENTITY`: create the column's sequence as an identity declaration does, then check the column and make it an identity column, and its partitions' columns with it.
pub(super) fn add_identity<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    target: IdentityTarget<'_>,
    name: &str,
    kind: AutoIncrementKind,
    declaration: Option<Box<IdentitySequenceDeclaration>>,
) -> Result<(), SQLError> {
    let table = target.table;
    let mut definition = column(context, table, name)?;
    let persistence = context
        .addition
        .generated
        .keys
        .catalog
        .table_persistence(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN identity", error))?
        .unwrap_or_default();
    let existing = definition.auto_increment.take();
    definition.auto_increment = Some(AutoIncrement {
        kind,
        sequence: None,
        owner: None,
        declaration,
    });
    crate::schema::sequences::implicit::materialize_implicit_sequences(
        &context.addition.sequences,
        "ALTER TABLE",
        table,
        std::slice::from_mut(&mut definition),
        persistence,
    )?;
    let identity = definition.auto_increment.take();
    definition.auto_increment = existing;
    set_identity_provenance(context, target, &definition, identity)
}

/// The checks `PostgreSQL` makes before it gives a column identity, then the change itself, here and in each partition.
fn set_identity_provenance<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    target: IdentityTarget<'_>,
    definition: &ColumnDef,
    identity: Option<AutoIncrement>,
) -> Result<(), SQLError> {
    let table = target.table;
    let name = definition.name.as_str();
    let (partitioned, partition) = partitioning(context, table)?;
    if partitioned && !target.recurse {
        return Err(only_partitioned_error(
            "cannot add identity to a column of only the partitioned table",
        ));
    }
    if partition && !target.recursing {
        return Err(partition_error(
            "cannot add identity to a column of a partition",
        ));
    }
    if !(definition.not_null || definition.primary_key) {
        return Err(column_error(
            "55000",
            table,
            name,
            "must be declared NOT NULL before identity can be added",
        )?);
    }
    if is_identity(definition) {
        return Err(column_error(
            "55000",
            table,
            name,
            "is already an identity column",
        )?);
    }
    if definition.default.is_some() || definition.generated.is_some() {
        return Err(column_error(
            "55000",
            table,
            name,
            "already has a default value",
        )?);
    }
    crate::schema::columns::alteration::set_auto_increment(
        &context.columns,
        table,
        name,
        identity.clone(),
    )?;
    if !target.recursing {
        crate::schema::sequences::ownership::attach_table_owners(
            &context.addition.ownership,
            table,
        )
        .map_err(|error| ddl_storage_error("ALTER COLUMN identity ownership", error))?;
    }
    if partitioned {
        for partition in partitions(context, table)? {
            let child = column(context, &partition, name)?;
            set_identity_provenance(
                context,
                IdentityTarget {
                    table: &partition,
                    recurse: true,
                    recursing: true,
                },
                &child,
                identity.clone(),
            )?;
        }
    }
    Ok(())
}

/// The sequence an identity column draws from, which a partition shares with its partitioned parent.
fn identity_sequence(column: &ColumnDef) -> Option<&str> {
    column
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
        .and_then(|provenance| provenance.sequence.as_deref())
}

/// `SET GENERATED`, `RESTART` and `SET sequence_option`: change an identity column's sequence as `ALTER SEQUENCE` would, then its generation, here and in each partition.
pub(super) fn set_identity<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    target: IdentityTarget<'_>,
    name: &str,
    kind: Option<AutoIncrementKind>,
    repeated_kind: bool,
    sequence: &SequenceDeclaration,
    error: Option<&DeferredSQLError>,
) -> Result<(), SQLError> {
    let table = target.table;
    let definition = column(context, table, name)?;
    if !target.recursing {
        if let Some(sequence_name) = identity_sequence(&definition) {
            if error.is_some() || *sequence != SequenceDeclaration::default() {
                alter_identity_sequence(&context.identities, sequence_name, sequence, error)?;
            }
        }
    }
    if repeated_kind {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "conflicting or redundant options".into(),
        });
    }
    let (partitioned, partition) = partitioning(context, table)?;
    if partitioned && !target.recurse {
        return Err(only_partitioned_error(
            "cannot change identity column of only the partitioned table",
        ));
    }
    if partition && !target.recursing {
        return Err(partition_error(
            "cannot change identity column of a partition",
        ));
    }
    if !is_identity(&definition) {
        return Err(column_error(
            "55000",
            table,
            name,
            "is not an identity column",
        )?);
    }
    let Some(kind) = kind else {
        return Ok(());
    };
    let mut provenance = definition
        .auto_increment
        .clone()
        .expect("an identity column has its provenance");
    provenance.kind = kind;
    crate::schema::columns::alteration::set_auto_increment(
        &context.columns,
        table,
        name,
        Some(provenance),
    )?;
    if partitioned {
        for partition in partitions(context, table)? {
            set_identity(
                context,
                IdentityTarget {
                    table: &partition,
                    recurse: true,
                    recursing: true,
                },
                name,
                Some(kind),
                false,
                &SequenceDeclaration::default(),
                None,
            )?;
        }
    }
    Ok(())
}

/// Change an identity sequence as `ALTER SEQUENCE` would: read the options in `PostgreSQL`'s order against its current definition and value, then apply them.
fn alter_identity_sequence(
    context: &IdentityAlterContext<'_>,
    name: &str,
    sequence: &SequenceDeclaration,
    error: Option<&DeferredSQLError>,
) -> Result<(), SQLError> {
    if let Some(error) = error {
        return Err(error.clone().into());
    }
    let relation = RelationIdentity::from_legacy_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
    let state = context
        .definitions
        .catalog
        .state(&relation)?
        .ok_or_else(|| SQLError::Internal(format!("identity sequence `{name}` disappeared")))?;
    let altered = uqa_sql::schema::sequences::declaration::alter_declared_sequence(
        sequence,
        &state.definition(),
        state.current,
        true,
    )?;
    let changed = altered.definition;
    let bound = |declared: bool, value: i64| {
        if declared {
            SequenceBound::Value(value)
        } else {
            SequenceBound::Unchanged
        }
    };
    let alter = AlterSequence {
        name: name.to_owned(),
        restart: sequence
            .restart
            .as_ref()
            .map_or(SequenceRestart::Unchanged, |_| {
                SequenceRestart::With(altered.current)
            }),
        increment: sequence.increment.as_ref().map(|_| changed.increment),
        start: sequence.start.as_ref().map(|_| changed.start),
        data_type: sequence.data_type.as_ref().map(|_| changed.data_type),
        min_value: bound(sequence.min_value.is_some(), changed.min_value),
        max_value: bound(sequence.max_value.is_some(), changed.max_value),
        cycle: sequence.cycle,
        cache_size: sequence.cache.as_ref().map(|_| changed.cache_size),
        ..AlterSequence::default()
    };
    crate::schema::sequences::alteration::alter_sequence_definition(
        &context.definitions,
        name,
        &relation,
        context.catalog.sequence_persistence(&relation),
        &alter,
    )?;
    Ok(())
}

/// Validate the identity sequence's type before column checks, as `PostgreSQL` prepares `ALTER SEQUENCE ... AS` first. The caller skips partitions, whose columns draw from their parent's sequence.
pub(super) fn validate_identity_type(
    context: &IdentityAlterContext<'_>,
    column: &ColumnDef,
    ty: &uqa_sql::ast::ColumnType,
) -> Result<(), SQLError> {
    let Some(name) = identity_sequence(column) else {
        return Ok(());
    };
    let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
    let state = context
        .definitions
        .catalog
        .state(&relation)?
        .ok_or_else(|| SQLError::Internal(format!("identity sequence `{name}` disappeared")))?;
    uqa_sql::schema::sequences::declaration::alter_declared_sequence(
        &SequenceDeclaration {
            data_type: Some(ty.clone()),
            ..SequenceDeclaration::default()
        },
        &state.definition(),
        state.current,
        true,
    )?;
    Ok(())
}

/// Publish the previously validated sequence type in the table's definition transaction.
pub(super) fn retype_identity_sequence<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    name: &str,
    ty: &uqa_sql::ast::ColumnType,
) -> Result<(), SQLError> {
    let (_, partition) = partitioning(context, table)?;
    if partition {
        return Ok(());
    }
    let definition = column(context, table, name)?;
    let Some(sequence_name) = identity_sequence(&definition) else {
        return Ok(());
    };
    alter_identity_sequence(
        &context.identities,
        sequence_name,
        &SequenceDeclaration {
            data_type: Some(ty.clone()),
            ..SequenceDeclaration::default()
        },
        None,
    )
}

/// `DROP IDENTITY [ IF EXISTS ]`: stop the column, and its partitions' columns, from drawing identity values, and drop the sequence. The column keeps its `NOT NULL`.
pub(super) fn drop_identity<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    target: IdentityTarget<'_>,
    name: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    let table = target.table;
    let (partitioned, partition) = partitioning(context, table)?;
    if partitioned && !target.recurse {
        return Err(only_partitioned_error(
            "cannot drop identity from a column of only the partitioned table",
        ));
    }
    if partition && !target.recursing {
        return Err(partition_error(
            "cannot drop identity from a column of a partition",
        ));
    }
    let definition = column(context, table, name)?;
    if !is_identity(&definition) {
        if if_exists {
            context
                .constraints
                .notices
                .push(uqa_sql::SQLNotice::notice(format!(
                    "column \"{name}\" of relation \"{}\" is not an identity column, skipping",
                    local_name(table)?
                )));
            return Ok(());
        }
        return Err(column_error(
            "55000",
            table,
            name,
            "is not an identity column",
        )?);
    }
    let sequence = identity_sequence(&definition).map(str::to_owned);
    crate::schema::columns::alteration::set_auto_increment(&context.columns, table, name, None)?;
    if partitioned {
        for partition in partitions(context, table)? {
            drop_identity(
                context,
                IdentityTarget {
                    table: &partition,
                    recurse: true,
                    recursing: true,
                },
                name,
                false,
            )?;
        }
    }
    if let (false, Some(sequence)) = (target.recursing, sequence) {
        drop_identity_sequence(context, &sequence)?;
    }
    Ok(())
}

/// The identity sequence stops belonging to its column: its owner, the internal dependency of the sequence on the column, is cleared.
fn release_identity_sequence(
    context: &IdentityAlterContext<'_>,
    name: &str,
    relation: &RelationIdentity,
) -> Result<(), SQLError> {
    let definitions = &context.definitions;
    let object_id = definitions.catalog.object_id(relation).ok_or_else(|| {
        SQLError::Internal(format!("identity sequence `{name}` has no object identity"))
    })?;
    let mut state = definitions
        .catalog
        .state(relation)?
        .ok_or_else(|| SQLError::Internal(format!("identity sequence `{name}` disappeared")))?;
    state.owner = None;
    definitions.publication.replace_sequence(
        name,
        relation,
        object_id,
        context.catalog.sequence_persistence(relation),
        state,
        true,
    )
}

/// `ATExecDropIdentity` removes the sequence's internal dependency on the column, as `deleteDependencyRecordsForClass` does, and then `performDeletion` drops the sequence as it drops any sequence, failing while another object depends on it.
fn drop_identity_sequence<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    sequence: &str,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(sequence)
        .map_err(|error| SQLError::Internal(format!("resolve sequence `{sequence}`: {error}")))?;
    release_identity_sequence(&context.identities, sequence, &relation)?;
    crate::schema::deletion::perform_deletion(
        &context.removal.deletion.catalog_removal_context(),
        |dependencies| {
            Ok(vec![crate::schema::deletion::required_address(
                dependencies.relation_address(&relation, None),
                || format!("sequence {sequence}"),
            )?])
        },
        false,
    )
}
