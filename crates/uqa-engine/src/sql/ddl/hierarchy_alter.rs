//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 ALTER inheritance and partition lifecycle.

use super::{ddl_storage_error, AlterTableAction, Engine, SQLError};
use uqa_sql::schema::inheritance::alter::{
    append_inherited_foreign_keys, append_inherited_keys, detached_bound_check,
    install_inherited_identity, normalize_parent_sequence_numbers,
    remove_partition_inherited_constraints, restore_identity_overrides,
};

use uqa_sql::ast::{AutoIncrement, ColumnDef, PartitionBound, TableHierarchy};

use crate::capabilities::RelationResolution;

pub(super) fn run_alter_hierarchy_action(
    engine: &Engine,
    table: &str,
    action: AlterTableAction,
) -> Result<(), SQLError> {
    match action {
        AlterTableAction::AddInheritance { parent } => add_inheritance(engine, table, &parent),
        AlterTableAction::DropInheritance { parent } => drop_inheritance(engine, table, &parent),
        AlterTableAction::AttachPartition { partition, bound } => {
            attach_partition(engine, table, &partition, bound)
        }
        AlterTableAction::DetachPartition {
            partition,
            concurrently,
            finalize,
        } => detach_partition(engine, table, &partition, concurrently, finalize),
        _ => Err(SQLError::Internal(
            "non-hierarchy ALTER action reached hierarchy executor".into(),
        )),
    }
}

fn add_inheritance(engine: &Engine, child: &str, requested_parent: &str) -> Result<(), SQLError> {
    let parent = resolve_table(engine, requested_parent)?;
    lock_secondary_relation(engine, child, &parent)?;
    validate_matching_persistence(engine, child, &parent, "inherit from")?;
    let mut hierarchy = read_hierarchy(engine, child)?;
    let parent_hierarchy = read_hierarchy(engine, &parent)?;
    if hierarchy.is_partition() {
        return Err(wrong_object("cannot change inheritance of a partition"));
    }
    if hierarchy.partition_spec.is_some() {
        return Err(wrong_object(
            "cannot change inheritance of partitioned table",
        ));
    }
    if parent_hierarchy.is_partition() {
        return Err(wrong_object("cannot inherit from a partition"));
    }
    if parent_hierarchy.partition_spec.is_some() {
        return Err(wrong_object(format!(
            "cannot inherit from partitioned table \"{requested_parent}\""
        )));
    }
    if hierarchy
        .parents
        .iter()
        .any(|candidate| candidate == &parent)
    {
        return Err(routine(
            "42P07",
            format!("relation \"{requested_parent}\" would be inherited from more than once"),
        ));
    }
    if child == parent
        || engine
            .hierarchy_scan_tables(child, true)?
            .iter()
            .any(|descendant| descendant == &parent)
    {
        return Err(routine("42P07", "circular inheritance not allowed"));
    }
    validate_row_type(engine, &parent, child, false, false)?;
    validate_inherited_checks(engine, &parent, child)?;
    normalize_parent_sequence_numbers(&mut hierarchy);
    let sequence_number = hierarchy.next_parent_sequence_number();
    hierarchy.parents.push(parent);
    hierarchy.parent_sequence_numbers.push(sequence_number);
    replace_hierarchy_only(engine, child, hierarchy, "ALTER TABLE INHERIT")
}

fn drop_inheritance(engine: &Engine, child: &str, requested_parent: &str) -> Result<(), SQLError> {
    let parent = resolve_table(engine, requested_parent)?;
    lock_secondary_relation(engine, child, &parent)?;
    let mut hierarchy = read_hierarchy(engine, child)?;
    if hierarchy.is_partition() {
        return Err(wrong_object("cannot change inheritance of a partition"));
    }
    if hierarchy.partition_spec.is_some() {
        return Err(wrong_object(
            "cannot change inheritance of partitioned table",
        ));
    }
    let Some(index) = hierarchy
        .parents
        .iter()
        .position(|candidate| candidate == &parent)
    else {
        return Err(routine(
            "42P01",
            format!(
                "relation \"{requested_parent}\" is not a parent of relation \"{}\"",
                local_relation_name(child)
            ),
        ));
    };
    normalize_parent_sequence_numbers(&mut hierarchy);
    hierarchy.parents.remove(index);
    hierarchy.parent_sequence_numbers.remove(index);
    replace_hierarchy_only(engine, child, hierarchy, "ALTER TABLE NO INHERIT")
}

fn attach_partition(
    engine: &Engine,
    parent: &str,
    requested_partition: &str,
    bound: PartitionBound,
) -> Result<(), SQLError> {
    let partition = resolve_table(engine, requested_partition)?;
    lock_secondary_relation(engine, parent, &partition)?;
    validate_matching_persistence(engine, &partition, parent, "attach to")?;
    let parent_hierarchy = read_hierarchy(engine, parent)?;
    let Some(parent_spec) = parent_hierarchy.partition_spec.as_ref() else {
        return Err(wrong_object(format!(
            "ALTER action ATTACH PARTITION cannot be performed on relation \"{}\"",
            local_relation_name(parent)
        )));
    };
    let partition_hierarchy = read_hierarchy(engine, &partition)?;
    if partition_hierarchy.is_partition() {
        return Err(wrong_object(format!(
            "\"{requested_partition}\" is already a partition"
        )));
    }
    if !partition_hierarchy.parents.is_empty() {
        return Err(wrong_object("cannot attach inheritance child as partition"));
    }
    let direct_children = engine.direct_hierarchy_children(&partition)?;
    if partition_hierarchy.partition_spec.is_none() && !direct_children.is_empty() {
        return Err(wrong_object(
            "cannot attach inheritance parent as partition",
        ));
    }
    if parent == partition
        || engine
            .hierarchy_scan_tables(&partition, true)?
            .iter()
            .any(|descendant| descendant == parent)
    {
        return Err(routine("42P07", "circular inheritance not allowed"));
    }
    validate_row_type(engine, parent, &partition, true, true)?;
    validate_inherited_checks(engine, parent, &partition)?;
    crate::sql::validate_new_partition_bound(engine, parent, &bound)?;
    validate_attached_rows(engine, parent, &partition, &bound)?;
    validate_default_partition_exclusion(engine, parent, &bound)?;

    let parent_columns = table_columns(engine, parent, "ATTACH PARTITION")?;
    let inherited_identity = parent_columns
        .iter()
        .filter_map(|column| {
            column
                .auto_increment
                .as_ref()
                .filter(|increment| increment.is_identity())
                .map(|increment| (column.name.clone(), increment.clone()))
        })
        .collect::<Vec<_>>();
    let parent_keys = engine
        .try_key_constraints(parent)
        .map_err(|error| ddl_storage_error("ATTACH PARTITION constraints", error))?;
    let parent_foreign_keys = engine
        .try_foreign_keys(parent)
        .map_err(|error| ddl_storage_error("ATTACH PARTITION constraints", error))?;
    let subtree = engine.hierarchy_scan_tables(&partition, true)?;
    for target in &subtree {
        let mut columns = table_columns(engine, target, "ATTACH PARTITION")?;
        let identity_overrides = install_inherited_identity(&mut columns, &inherited_identity)?;
        let mut constraints = declared_constraints(engine, target, "ATTACH PARTITION")?;
        let inherited_keys = append_inherited_keys(&mut constraints.key_constraints, &parent_keys);
        let inherited_foreign_keys =
            append_inherited_foreign_keys(&mut constraints.foreign_keys, &parent_foreign_keys);
        let mut hierarchy = constraints.hierarchy.clone();
        hierarchy.partition_identity_overrides = identity_overrides;
        hierarchy.partition_inherited_key_constraints = inherited_keys;
        hierarchy.partition_inherited_foreign_keys = inherited_foreign_keys;
        if target == &partition {
            hierarchy.parents = vec![parent.to_string()];
            hierarchy.parent_sequence_numbers = vec![1];
            hierarchy.partition_bound = Some(bound.clone());
        }
        engine
            .replace_table_hierarchy_components(
                target,
                columns,
                constraints.checks,
                constraints.foreign_keys,
                constraints.key_constraints,
                hierarchy,
            )
            .map_err(|error| ddl_storage_error("ATTACH PARTITION", error))?;
    }
    for target in subtree {
        validate_existing_constraints(engine, &target)?;
    }
    let _ = parent_spec;
    Ok(())
}

fn detach_partition(
    engine: &Engine,
    parent: &str,
    requested_partition: &str,
    concurrently: bool,
    finalize: bool,
) -> Result<(), SQLError> {
    let partition = resolve_table(engine, requested_partition)?;
    lock_secondary_relation(engine, parent, &partition)?;
    let parent_hierarchy = read_hierarchy(engine, parent)?;
    let Some(parent_spec) = parent_hierarchy.partition_spec.as_ref() else {
        return Err(wrong_object(format!(
            "ALTER action DETACH PARTITION cannot be performed on relation \"{}\"",
            local_relation_name(parent)
        )));
    };
    let partition_hierarchy = read_hierarchy(engine, &partition)?;
    let attached = partition_hierarchy.is_partition()
        && partition_hierarchy
            .parents
            .first()
            .is_some_and(|edge| edge == parent);
    if finalize {
        return Err(routine(
            "55000",
            format!(
                "cannot complete detaching partition \"{}\"\nDETAIL: There's no pending concurrent detach.",
                local_relation_name(&partition)
            ),
        ));
    }
    if !attached {
        return Err(routine(
            "42P01",
            format!(
                "relation \"{requested_partition}\" is not a partition of relation \"{}\"",
                local_relation_name(parent)
            ),
        ));
    }
    if concurrently && direct_default_partition(engine, parent)?.is_some() {
        return Err(routine(
            "55000",
            "cannot detach partitions concurrently when a default partition exists",
        ));
    }
    let bound = partition_hierarchy
        .partition_bound
        .as_ref()
        .ok_or_else(|| SQLError::Internal("attached partition lost its bound".into()))?
        .clone();
    let inherited_identity = table_columns(engine, parent, "DETACH PARTITION")?
        .into_iter()
        .filter_map(|column| {
            column
                .auto_increment
                .filter(AutoIncrement::is_identity)
                .map(|increment| (column.name, increment))
        })
        .collect::<Vec<_>>();
    let subtree = engine.hierarchy_scan_tables(&partition, true)?;
    for target in &subtree {
        let mut columns = table_columns(engine, target, "DETACH PARTITION")?;
        let mut constraints = declared_constraints(engine, target, "DETACH PARTITION")?;
        restore_identity_overrides(
            &mut columns,
            &inherited_identity,
            &constraints.hierarchy.partition_identity_overrides,
        );
        remove_partition_inherited_constraints(&mut constraints);
        if concurrently {
            constraints.checks.push(detached_bound_check(
                target,
                parent_spec,
                &bound,
                &constraints.checks,
            ));
        }
        let mut hierarchy = constraints.hierarchy.clone();
        hierarchy.partition_identity_overrides.clear();
        hierarchy.partition_inherited_key_constraints.clear();
        hierarchy.partition_inherited_foreign_keys.clear();
        if target == &partition {
            hierarchy.parents.clear();
            hierarchy.parent_sequence_numbers.clear();
            hierarchy.partition_bound = None;
        }
        engine
            .replace_table_hierarchy_components(
                target,
                columns,
                constraints.checks,
                constraints.foreign_keys,
                constraints.key_constraints,
                hierarchy,
            )
            .map_err(|error| ddl_storage_error("DETACH PARTITION", error))?;
    }
    Ok(())
}

fn validate_row_type(
    engine: &Engine,
    parent: &str,
    child: &str,
    exact_columns: bool,
    reject_child_identity: bool,
) -> Result<(), SQLError> {
    let parent_columns = table_columns(engine, parent, "ALTER TABLE hierarchy")?;
    let child_columns = table_columns(engine, child, "ALTER TABLE hierarchy")?;
    uqa_sql::schema::inheritance::alter::validate_row_type(
        &parent_columns,
        &child_columns,
        parent,
        child,
        exact_columns,
        reject_child_identity,
    )
}

fn validate_inherited_checks(engine: &Engine, parent: &str, child: &str) -> Result<(), SQLError> {
    let child_columns = engine
        .try_describe_table(child)
        .map_err(|error| ddl_storage_error("read child CHECK columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(child.to_string()))?;
    let parent_checks = engine
        .try_check_constraint_definitions(parent)
        .map_err(|error| ddl_storage_error("read parent CHECK constraints", error))?;
    let child_checks = engine
        .try_check_constraint_definitions(child)
        .map_err(|error| ddl_storage_error("read child CHECK constraints", error))?;
    uqa_sql::schema::inheritance::alter::validate_inherited_checks(
        child,
        &child_columns,
        &parent_checks,
        &child_checks,
    )
}

fn validate_attached_rows(
    engine: &Engine,
    parent: &str,
    partition: &str,
    bound: &PartitionBound,
) -> Result<(), SQLError> {
    for physical_table in engine.hierarchy_scan_tables(partition, true)? {
        for doc_id in engine.live_table_doc_ids(&physical_table)? {
            let Some(document) = engine.get_document(&physical_table, doc_id)? else {
                continue;
            };
            if !crate::sql::prospective_partition_bound_accepts_document(
                engine, parent, bound, &document,
            )? {
                return Err(routine(
                    "23514",
                    format!(
                        "partition constraint of relation \"{}\" is violated by some row",
                        local_relation_name(&physical_table)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_default_partition_exclusion(
    engine: &Engine,
    parent: &str,
    new_bound: &PartitionBound,
) -> Result<(), SQLError> {
    if matches!(new_bound, PartitionBound::Default) {
        return Ok(());
    }
    let Some(default) = direct_default_partition(engine, parent)? else {
        return Ok(());
    };
    for physical_table in engine.hierarchy_scan_tables(&default, true)? {
        for doc_id in engine.live_table_doc_ids(&physical_table)? {
            let Some(document) = engine.get_document(&physical_table, doc_id)? else {
                continue;
            };
            if crate::sql::prospective_partition_bound_accepts_document(
                engine, parent, new_bound, &document,
            )? {
                return Err(routine(
                    "23514",
                    format!(
                        "updated partition constraint for default partition \"{}\" would be violated by some row",
                        local_relation_name(&default)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_existing_constraints(engine: &Engine, table: &str) -> Result<(), SQLError> {
    for doc_id in engine.live_table_doc_ids(table)? {
        let Some(document) = engine.get_document(table, doc_id)? else {
            continue;
        };
        crate::sql::dml::validate_document_constraints(
            engine,
            table,
            &document,
            &[],
            Some(doc_id),
        )?;
    }
    Ok(())
}

fn direct_default_partition(engine: &Engine, parent: &str) -> Result<Option<String>, SQLError> {
    for child in engine.direct_hierarchy_children(parent)? {
        if matches!(
            read_hierarchy(engine, &child)?.partition_bound,
            Some(PartitionBound::Default)
        ) {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn replace_hierarchy_only(
    engine: &Engine,
    table: &str,
    hierarchy: TableHierarchy,
    action: &str,
) -> Result<(), SQLError> {
    let columns = table_columns(engine, table, action)?;
    let constraints = declared_constraints(engine, table, action)?;
    engine
        .replace_table_hierarchy_components(
            table,
            columns,
            constraints.checks,
            constraints.foreign_keys,
            constraints.key_constraints,
            hierarchy,
        )
        .map_err(|error| ddl_storage_error(action, error))
}

fn table_columns(engine: &Engine, table: &str, action: &str) -> Result<Vec<ColumnDef>, SQLError> {
    engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error(action, error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
}

fn declared_constraints(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<uqa_sql::ast::TableConstraintSet, SQLError> {
    engine
        .try_declared_table_constraints(table)
        .map_err(|error| ddl_storage_error(action, error))
}

fn read_hierarchy(engine: &Engine, table: &str) -> Result<TableHierarchy, SQLError> {
    engine
        .try_table_hierarchy(table)
        .map_err(|error| SQLError::Internal(format!("read table hierarchy: {error}")))
}

fn resolve_table(engine: &Engine, requested: &str) -> Result<String, SQLError> {
    match engine.resolve_visible_relation_kind(requested)? {
        RelationResolution::Found(canonical, "table") => Ok(canonical),
        RelationResolution::Found(canonical, kind) => Err(wrong_object(format!(
            "relation \"{canonical}\" is a {kind}, not a table"
        ))),
        RelationResolution::MissingSchema(schema) => Err(routine(
            "3F000",
            format!("schema \"{schema}\" does not exist"),
        )),
        RelationResolution::MissingRelation => Err(routine(
            "42P01",
            format!("relation \"{requested}\" does not exist"),
        )),
    }
}

fn lock_secondary_relation(
    engine: &Engine,
    primary: &str,
    secondary: &str,
) -> Result<(), SQLError> {
    if primary != secondary {
        engine.lock_relation(
            secondary,
            crate::row_locks::RelationLockMode::AccessExclusive,
        )?;
    }
    Ok(())
}

fn validate_matching_persistence(
    engine: &Engine,
    child: &str,
    parent: &str,
    operation: &str,
) -> Result<(), SQLError> {
    let child_persistence = engine
        .table_persistence(child)
        .map_err(|error| ddl_storage_error("read child persistence", error))?
        .ok_or_else(|| SQLError::UnknownTable(child.to_string()))?;
    let parent_persistence = engine
        .table_persistence(parent)
        .map_err(|error| ddl_storage_error("read parent persistence", error))?
        .ok_or_else(|| SQLError::UnknownTable(parent.to_string()))?;
    uqa_sql::schema::inheritance::alter::validate_matching_persistence(
        child,
        parent,
        operation,
        child_persistence,
        parent_persistence,
    )
}

fn local_relation_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn wrong_object(message: impl Into<String>) -> SQLError {
    routine("42809", message)
}

fn routine(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
