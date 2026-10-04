//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE inheritance and partition row-type preparation.

use crate::semantics::partition::{
    transform_partition_bound, validate_new_partition_bound, PartitionContext,
};
use crate::{
    ast::{CreateTable, TableCheck, TableConstraintSet},
    SQLError,
};
/// Parent lookup and constraint declarations used while assembling a new row type.
pub trait InheritanceCatalog {
    fn resolve_parent(&self, name: &str) -> Result<String, SQLError>;
    fn declared_constraints(&self, table: &str) -> Result<TableConstraintSet, String>;
    fn check_definitions(&self, table: &str) -> Result<Vec<TableCheck>, String>;
    /// The key attributes of each unique index of `table` that no key constraint owns; a partition builds an index for each.
    fn unique_index_keys(&self, table: &str) -> Result<Vec<Vec<crate::ast::IndexKey>>, String>;
}
pub struct InheritanceContext<'a> {
    pub catalog: &'a dyn InheritanceCatalog,
    pub partitions: PartitionContext<'a>,
    pub roles: &'a dyn crate::expr::EngineHook,
}

/// The parent relation that `INHERITS` names: `table_openrv` refuses indexes and composite types, and `MergeAttributes` accepts only tables. Foreign tables are not inheritance parents here.
pub fn inheritance_parent_target(
    resolution: crate::catalog::resolution::RelationResolution,
    requested: &str,
) -> Result<String, SQLError> {
    use crate::catalog::resolution::RelationResolution;
    match resolution {
        RelationResolution::Found(canonical, "table") => Ok(canonical),
        RelationResolution::Found(canonical, kind @ ("index" | "composite type")) => {
            Err(crate::catalog::analysis::UnopenableRelation {
                name: local_relation_name(&canonical),
                kinds: if kind == "index" {
                    "indexes"
                } else {
                    "composite types"
                },
            }
            .error())
        }
        RelationResolution::Found(canonical, "view" | "materialized view" | "sequence") => {
            Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "inherited relation \"{}\" is not a table or foreign table",
                    local_relation_name(&canonical)
                ),
            })
        }
        RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::Found(_, _) | RelationResolution::MissingRelation => {
            Err(SQLError::UnknownTable(requested.to_string()))
        }
    }
}

fn local_relation_name(canonical: &str) -> String {
    uqa_core::RelationIdentity::from_legacy_name(canonical)
        .map_or_else(|_| canonical.to_string(), |relation| relation.name)
}

/// `MergeAttributes`: the columns, CHECK constraints and partition keys a new table inherits from its parents, ahead of its own. The table's partition key and bound are bound by [`bind_create_table_partitioning`] once its row type is described, as `DefineRelation` computes them after creating the relation.
#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub fn merge_create_table_hierarchy(
    context: &InheritanceContext<'_>,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    table.hierarchy.local_columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect();
    if table.hierarchy.parents.is_empty() {
        if table.hierarchy.partition_bound.is_some() {
            return Err(SQLError::Internal(
                "partition bound has no parent relation".into(),
            ));
        }
        return Ok(());
    }
    let is_partition = table.hierarchy.partition_bound.is_some();
    if is_partition && table.hierarchy.parents.len() != 1 {
        return Err(SQLError::Internal(
            "a partition must have exactly one parent".into(),
        ));
    }
    let mut canonical_parents = Vec::with_capacity(table.hierarchy.parents.len());
    let mut inherited_columns = Vec::new();
    let mut inherited_checks = Vec::new();
    let mut inherited_foreign_keys = Vec::new();
    let mut inherited_keys = Vec::new();
    for requested_parent in &table.hierarchy.parents {
        // A parent with the new table's own name is the relation that already has the name, which `heap_create_with_catalog` reports once the columns are described.
        let parent = context.catalog.resolve_parent(requested_parent)?;
        let parent_hierarchy = context
            .partitions
            .catalog
            .try_table_hierarchy(&parent)
            .map_err(|error| SQLError::Internal(format!("read parent hierarchy: {error}")))?;
        if is_partition {
            if parent_hierarchy.partition_spec.is_none() {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("relation \"{requested_parent}\" is not partitioned"),
                });
            }
        } else if parent_hierarchy.partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot inherit from partitioned table \"{requested_parent}\""),
            });
        }
        let mut columns = context
            .partitions
            .catalog
            .try_describe_table(&parent)
            .map_err(|error| SQLError::Internal(format!("read inherited row type: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?;
        for column in &mut columns {
            column.not_null_identity = None;
            // The child stores its own copy of an inherited default.
            column.default_catalog_oid = None;
            if let Some(reference) = &mut column.references {
                reference.catalog_identity = None;
                reference.referenced_partitions.clear();
            }
            if column.not_null_no_inherit {
                column.not_null = false;
                column.not_null_explicit = false;
                column.not_null_name = None;
                column.not_null_no_inherit = false;
                column.not_null_validated = true;
            }
            column.not_null_is_local = !column.not_null;
            // CHECKs inherit as named constraints independently of the merged column's origin.
            column.check = None;
            column.check_name = None;
            column.check_object_id = None;
            column.check_catalog_oid = None;
            column.check_is_local = true;
            column.check_enforced = true;
            column.check_validated = true;
            column.check_no_inherit = false;
        }
        if !is_partition {
            // PostgreSQL inherits the NOT NULL property of an identity column, but not its identity generation attribute or owned sequence. SERIAL is different: its nextval default is ordinary inherited metadata and therefore keeps pointing at the parent's sequence. Key constraints, like foreign keys, are not inherited: only their columns' NOT NULL constraints are.
            for column in &mut columns {
                column.references = None;
                column.primary_key = false;
                column.unique = false;
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(crate::ast::AutoIncrement::is_identity)
                {
                    column.auto_increment = None;
                }
            }
        }
        merge_columns(&mut inherited_columns, columns)?;
        let constraints = context
            .catalog
            .declared_constraints(&parent)
            .map_err(|error| SQLError::Internal(format!("read inherited constraints: {error}")))?;
        for mut check in context
            .catalog
            .check_definitions(&parent)
            .map_err(|error| SQLError::Internal(format!("read inherited CHECKs: {error}")))?
            .into_iter()
            .filter(|check| !check.no_inherit)
        {
            super::check_inheritance::bind_parent_check_columns(&parent, &mut check.expr)?;
            check.is_local = false;
            check.object_id = None;
            check.catalog_oid = None;
            check.validated = check.enforced;
            inherited_checks.push(check);
        }
        if is_partition {
            inherited_foreign_keys.extend(constraints.foreign_keys.into_iter().map(|mut key| {
                key.catalog_identity = None;
                key.referenced_partitions.clear();
                key
            }));
            inherited_keys.extend(constraints.key_constraints.into_iter().map(|mut key| {
                key.name = None;
                key.catalog_identity = None;
                key
            }));
        }
        canonical_parents.push(parent);
    }
    merge_columns(&mut inherited_columns, std::mem::take(&mut table.columns))?;
    table.columns = inherited_columns;
    inherited_checks.append(&mut table.checks);
    table.checks = inherited_checks;
    if is_partition {
        inherited_foreign_keys.append(&mut table.foreign_keys);
        inherited_keys.append(&mut table.key_constraints);
        table.foreign_keys = inherited_foreign_keys;
        table.key_constraints = inherited_keys;
    }
    table.hierarchy.parents = canonical_parents;
    Ok(())
}

/// The bound of a new partition and the partition key of a new partitioned table, in `DefineRelation` order: `transformPartitionBound` and `check_new_partition_bound` before `ComputePartitionAttrs`.
pub fn bind_create_table_partitioning(
    context: &InheritanceContext<'_>,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    if let (Some(parent), Some(bound)) = (
        table.hierarchy.parents.first(),
        table.hierarchy.partition_bound.as_ref(),
    ) {
        // The stored bound holds the values coerced to the parent's key types, as PostgreSQL stores Const nodes.
        let bound = transform_partition_bound(&context.partitions, parent, bound)?;
        validate_new_partition_bound(&context.partitions, parent, &table.name, &bound)?;
        table.hierarchy.partition_bound = Some(bound);
    }
    validate_partition_keys(context, table)
}

fn merge_columns(
    merged: &mut Vec<crate::ast::ColumnDef>,
    incoming: Vec<crate::ast::ColumnDef>,
) -> Result<(), SQLError> {
    for column in incoming {
        if let Some(existing) = merged.iter_mut().find(|item| item.name == column.name) {
            merge_same_column(existing, column)?;
        } else {
            merged.push(column);
        }
    }
    Ok(())
}

pub fn merge_same_column(
    inherited: &mut crate::ast::ColumnDef,
    declared: crate::ast::ColumnDef,
) -> Result<(), SQLError> {
    if inherited.ty != declared.ty {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!(
                "inherited column \"{}\" has a type conflict",
                inherited.name
            ),
        });
    }
    if inherited.generated.is_some() != declared.generated.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!(
                "inherited column \"{}\" has a generation conflict",
                inherited.name
            ),
        });
    }
    let not_null_is_local = (inherited.not_null && inherited.not_null_is_local)
        || (declared.not_null && declared.not_null_is_local);
    if declared.not_null && (!inherited.not_null || declared.not_null_is_local) {
        inherited.not_null_name.clone_from(&declared.not_null_name);
        inherited.not_null_identity = declared.not_null_identity;
        inherited.not_null_validated = declared.not_null_validated;
        inherited.not_null_no_inherit = declared.not_null_no_inherit;
    }
    inherited.not_null |= declared.not_null;
    inherited.not_null_is_local = !inherited.not_null || not_null_is_local;
    inherited.not_null_explicit |= declared.not_null_explicit;
    inherited.primary_key |= declared.primary_key;
    inherited.unique |= declared.unique;
    if declared.auto_increment.is_some() {
        inherited.auto_increment = declared.auto_increment;
    }
    if declared.default.is_some() {
        inherited.default = declared.default;
    }
    if declared.generated.is_some() {
        inherited.generated = declared.generated;
    }
    if declared.check.is_some() {
        inherited.check = declared.check;
        inherited.check_name = declared.check_name;
        inherited.check_enforced = declared.check_enforced;
        inherited.check_validated = declared.check_validated;
        inherited.check_no_inherit = declared.check_no_inherit;
        inherited.check_is_local = declared.check_is_local;
        inherited.check_object_id = declared.check_object_id;
    }
    if declared.references.is_some() {
        inherited.references = declared.references;
    }
    Ok(())
}

mod partition_keys;
use partition_keys::validate_partition_keys;

pub mod alter;

pub mod detachment;
pub mod foreign_keys;
pub mod origins;
pub mod restoration;
