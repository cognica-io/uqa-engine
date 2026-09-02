//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 ALTER inheritance and partition lifecycle.

use super::{ddl_storage_error, AlterTableAction, Engine, SQLError, Value};
use uqa_sql::ast::{
    AutoIncrement, BinaryOp, ColumnDef, DetachedPartitionConstraint, Expr, ForeignKey,
    PartitionBound, PartitionIdentityOverride, PartitionRangeDatum, PartitionSpec,
    RelationPersistence, TableCheck, TableHierarchy, TableKeyConstraint,
};

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

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn validate_row_type(
    engine: &Engine,
    parent: &str,
    child: &str,
    exact_columns: bool,
    reject_child_identity: bool,
) -> Result<(), SQLError> {
    let parent_columns = table_columns(engine, parent, "ALTER TABLE hierarchy")?;
    let child_columns = table_columns(engine, child, "ALTER TABLE hierarchy")?;
    if reject_child_identity {
        if let Some(column) = child_columns.iter().find(|column| {
            column
                .auto_increment
                .as_ref()
                .is_some_and(AutoIncrement::is_identity)
        }) {
            return Err(routine(
                "55000",
                format!(
                    "table \"{}\" being attached contains an identity column \"{}\"\nDETAIL: The new partition may not contain an identity column.",
                    local_relation_name(child),
                    column.name
                ),
            ));
        }
    }
    for parent_column in &parent_columns {
        let Some(child_column) = child_columns
            .iter()
            .find(|column| column.name == parent_column.name)
        else {
            return Err(routine(
                "42804",
                format!("child table is missing column \"{}\"", parent_column.name),
            ));
        };
        if parent_column.ty != child_column.ty {
            return Err(routine(
                "42804",
                format!(
                    "child table \"{}\" has different type for column \"{}\"",
                    local_relation_name(child),
                    parent_column.name
                ),
            ));
        }
        if parent_column.not_null && !child_column.not_null {
            return Err(routine(
                "42804",
                format!(
                    "column \"{}\" in child table \"{}\" must be marked NOT NULL",
                    parent_column.name,
                    local_relation_name(child)
                ),
            ));
        }
        match (&parent_column.generated, &child_column.generated) {
            (None, None) | (Some(_), Some(_)) => {}
            (Some(_), None) => {
                return Err(routine(
                    "42804",
                    format!(
                        "column \"{}\" in child table must be a generated column",
                        parent_column.name
                    ),
                ))
            }
            (None, Some(_)) => {
                return Err(routine(
                    "42804",
                    format!(
                        "column \"{}\" in child table must not be a generated column",
                        parent_column.name
                    ),
                ))
            }
        }
        if let (Some(parent_generated), Some(child_generated)) =
            (&parent_column.generated, &child_column.generated)
        {
            if parent_generated.kind != child_generated.kind {
                return Err(routine(
                    "42804",
                    format!(
                        "column \"{}\" inherits from generated column of different kind",
                        parent_column.name
                    ),
                ));
            }
        }
    }
    if exact_columns {
        if let Some(extra) = child_columns.iter().find(|child_column| {
            !parent_columns
                .iter()
                .any(|parent_column| parent_column.name == child_column.name)
        }) {
            return Err(routine(
                "42804",
                format!(
                    "table \"{}\" contains column \"{}\" not found in parent \"{}\"\nDETAIL: The new partition may contain only the columns present in parent.",
                    local_relation_name(child),
                    extra.name,
                    local_relation_name(parent)
                ),
            ));
        }
    }
    Ok(())
}

fn validate_inherited_checks(engine: &Engine, parent: &str, child: &str) -> Result<(), SQLError> {
    let parent_checks = engine
        .try_check_constraint_definitions(parent)
        .map_err(|error| ddl_storage_error("read parent CHECK constraints", error))?;
    let child_checks = engine
        .try_check_constraint_definitions(child)
        .map_err(|error| ddl_storage_error("read child CHECK constraints", error))?;
    for parent_check in parent_checks
        .iter()
        .filter(|constraint| !constraint.no_inherit)
    {
        let Some(name) = parent_check.name.as_deref() else {
            return Err(SQLError::Internal(
                "persisted parent CHECK constraint has no name".into(),
            ));
        };
        let Some(child_check) = child_checks
            .iter()
            .find(|constraint| constraint.name.as_deref() == Some(name))
        else {
            return Err(routine(
                "42804",
                format!("child table is missing constraint \"{name}\""),
            ));
        };
        if child_check.no_inherit {
            return Err(routine(
                "42P17",
                format!(
                    "constraint \"{name}\" conflicts with non-inherited constraint on child table \"{}\"",
                    local_relation_name(child)
                ),
            ));
        }
        if child_check.enforced != parent_check.enforced
            || serde_json::to_value(&child_check.expr)
                .map_err(|error| SQLError::Internal(format!("serialize child CHECK: {error}")))?
                != serde_json::to_value(&parent_check.expr).map_err(|error| {
                    SQLError::Internal(format!("serialize parent CHECK: {error}"))
                })?
        {
            return Err(routine(
                "42804",
                format!(
                    "child table \"{}\" has different definition for check constraint \"{name}\"",
                    local_relation_name(child)
                ),
            ));
        }
    }
    Ok(())
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

fn install_inherited_identity(
    columns: &mut [ColumnDef],
    inherited: &[(String, AutoIncrement)],
) -> Result<Vec<PartitionIdentityOverride>, SQLError> {
    let mut overrides = Vec::with_capacity(inherited.len());
    for (name, increment) in inherited {
        let column = columns
            .iter_mut()
            .find(|column| column.name == *name)
            .ok_or_else(|| SQLError::Internal(format!("partition lost column `{name}`")))?;
        overrides.push(PartitionIdentityOverride {
            column: name.clone(),
            original: column.auto_increment.clone(),
        });
        column.auto_increment = Some(increment.clone());
    }
    Ok(overrides)
}

fn restore_identity_overrides(
    columns: &mut [ColumnDef],
    inherited: &[(String, AutoIncrement)],
    overrides: &[PartitionIdentityOverride],
) {
    for (name, _) in inherited {
        let Some(column) = columns.iter_mut().find(|column| column.name == *name) else {
            continue;
        };
        column.auto_increment = overrides
            .iter()
            .find(|identity_override| identity_override.column == *name)
            .and_then(|identity_override| identity_override.original.clone());
    }
}

fn append_inherited_keys(
    target: &mut Vec<TableKeyConstraint>,
    inherited: &[TableKeyConstraint],
) -> Vec<TableKeyConstraint> {
    let mut appended = Vec::new();
    for constraint in inherited {
        if target
            .iter()
            .any(|candidate| key_equivalent(candidate, constraint))
        {
            continue;
        }
        let mut constraint = constraint.clone();
        constraint.name = None;
        target.push(constraint.clone());
        appended.push(constraint);
    }
    appended
}

fn key_equivalent(left: &TableKeyConstraint, right: &TableKeyConstraint) -> bool {
    left.kind == right.kind
        && left.columns == right.columns
        && left.nulls_not_distinct == right.nulls_not_distinct
}

fn append_inherited_foreign_keys(
    target: &mut Vec<ForeignKey>,
    inherited: &[ForeignKey],
) -> Vec<ForeignKey> {
    let mut appended = Vec::new();
    for constraint in inherited {
        if !target
            .iter()
            .any(|candidate| foreign_key_equivalent(candidate, constraint))
        {
            target.push(constraint.clone());
            appended.push(constraint.clone());
        }
    }
    appended
}

fn remove_partition_inherited_constraints(constraints: &mut uqa_sql::ast::TableConstraintSet) {
    for inherited in &constraints.hierarchy.partition_inherited_key_constraints {
        if let Some(index) = constraints
            .key_constraints
            .iter()
            .position(|constraint| constraint == inherited)
        {
            constraints.key_constraints.remove(index);
        }
    }
    for inherited in &constraints.hierarchy.partition_inherited_foreign_keys {
        if let Some(index) = constraints
            .foreign_keys
            .iter()
            .position(|constraint| constraint == inherited)
        {
            constraints.foreign_keys.remove(index);
        }
    }
}

fn foreign_key_equivalent(left: &ForeignKey, right: &ForeignKey) -> bool {
    left.local_columns == right.local_columns
        && left.ref_table == right.ref_table
        && left.ref_columns == right.ref_columns
        && left.on_update == right.on_update
        && left.on_delete == right.on_delete
        && left.on_delete_set_columns == right.on_delete_set_columns
        && left.match_type == right.match_type
        && left.enforced == right.enforced
}

fn detached_bound_check(
    table: &str,
    spec: &PartitionSpec,
    bound: &PartitionBound,
    existing: &[TableCheck],
) -> TableCheck {
    let expr = renderable_bound_expression(spec, bound);
    let relation = local_relation_name(table);
    let key = spec.keys.first().and_then(|key| match key {
        Expr::Column(column) => Some(column.as_str()),
        _ => None,
    });
    let base = key.map_or_else(
        || format!("{relation}_check"),
        |column| format!("{relation}_{column}_check"),
    );
    let name = unique_constraint_name(&base, existing);
    TableCheck {
        name: Some(name),
        expr,
        enforced: true,
        validated: true,
        no_inherit: false,
        partition_constraint: Some(DetachedPartitionConstraint {
            spec: spec.clone(),
            bound: bound.clone(),
        }),
    }
}

fn unique_constraint_name(base: &str, existing: &[TableCheck]) -> String {
    if !existing
        .iter()
        .any(|constraint| constraint.name.as_deref() == Some(base))
    {
        return base.to_string();
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        if !existing
            .iter()
            .any(|constraint| constraint.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("u64 constraint suffix space is exhaustive")
}

fn renderable_bound_expression(spec: &PartitionSpec, bound: &PartitionBound) -> Expr {
    let Some(key) = spec.keys.first().cloned().filter(|_| spec.keys.len() == 1) else {
        return Expr::Literal(Value::Bool(true));
    };
    match bound {
        PartitionBound::List(values) => {
            let mut terms = Vec::new();
            let mut non_null = Vec::new();
            for value in values {
                if matches!(value, Expr::Literal(Value::Null)) {
                    terms.push(Expr::IsNull {
                        expr: Box::new(key.clone()),
                        negated: false,
                    });
                } else {
                    non_null.push(value.clone());
                }
            }
            if !non_null.is_empty() {
                terms.push(Expr::InList {
                    expr: Box::new(key),
                    list: non_null,
                    negated: false,
                });
            }
            if terms.len() == 1 {
                terms.pop().unwrap_or(Expr::Literal(Value::Bool(true)))
            } else {
                Expr::Or(terms)
            }
        }
        PartitionBound::Range { lower, upper } if lower.len() == 1 && upper.len() == 1 => {
            let mut terms = vec![Expr::IsNull {
                expr: Box::new(key.clone()),
                negated: true,
            }];
            if let PartitionRangeDatum::Value(lower) = &lower[0] {
                terms.push(Expr::Binary {
                    op: BinaryOp::GreaterEqual,
                    lhs: Box::new(key.clone()),
                    rhs: Box::new(lower.clone()),
                });
            }
            if let PartitionRangeDatum::Value(upper) = &upper[0] {
                terms.push(Expr::Binary {
                    op: BinaryOp::Less,
                    lhs: Box::new(key),
                    rhs: Box::new(upper.clone()),
                });
            }
            Expr::And(terms)
        }
        PartitionBound::Hash { .. } | PartitionBound::Range { .. } | PartitionBound::Default => {
            Expr::Literal(Value::Bool(true))
        }
    }
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
    match engine.try_resolve_visible_relation_kind(requested)? {
        Some((canonical, "table")) => Ok(canonical),
        Some((canonical, kind)) => Err(wrong_object(format!(
            "relation \"{canonical}\" is a {kind}, not a table"
        ))),
        None => Err(routine(
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
    if (child_persistence == RelationPersistence::Temporary)
        != (parent_persistence == RelationPersistence::Temporary)
    {
        return Err(wrong_object(format!(
            "cannot {operation} {} relation \"{}\" from {} relation \"{}\"",
            persistence_label(child_persistence),
            local_relation_name(child),
            persistence_label(parent_persistence),
            local_relation_name(parent)
        )));
    }
    Ok(())
}

fn persistence_label(persistence: RelationPersistence) -> &'static str {
    match persistence {
        RelationPersistence::Temporary => "temporary",
        RelationPersistence::Unlogged => "unlogged",
        RelationPersistence::Permanent => "permanent",
    }
}

fn normalize_parent_sequence_numbers(hierarchy: &mut TableHierarchy) {
    if hierarchy.parent_sequence_numbers.len() == hierarchy.parents.len() {
        return;
    }
    hierarchy.parent_sequence_numbers = hierarchy
        .parents
        .iter()
        .enumerate()
        .map(|(index, _)| i32::try_from(index + 1).unwrap_or(i32::MAX))
        .collect();
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
