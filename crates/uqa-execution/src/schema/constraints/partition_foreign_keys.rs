//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The foreign keys the partitions of a partitioned table hold for one of its foreign keys, which `PostgreSQL`'s `addFkRecurseReferencing` and `CloneFkReferencing` attach or copy and `QueueFKConstraintValidation` validates partition by partition.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    ddl_storage_error, publish_constraint_state, table_constraint_state, ConstraintAlterContext,
};
use crate::mutation::constraints::context::ConstraintContext;
use uqa_sql::ast::{ColumnDef, ForeignKey, TableConstraintSet};
use uqa_sql::schema::inheritance::foreign_keys::{
    attachable_foreign_key, declared_foreign_key_families, partition_foreign_key_copy,
    rejoin_foreign_key_families, DeclaredForeignKey,
};
use uqa_sql::semantics::partition::PartitionContext;
use uqa_sql::SQLError;

/// The relation state that partition foreign key changes read and publish.
pub trait PartitionForeignKeyTables {
    fn declared(&self, table: &str) -> Result<(Vec<ColumnDef>, TableConstraintSet), SQLError>;
    /// The table's foreign keys, column foreign keys included, with their stored targets bound.
    fn bound_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, SQLError>;
    fn publish(
        &self,
        table: &str,
        columns: Vec<ColumnDef>,
        constraints: TableConstraintSet,
    ) -> Result<(), SQLError>;
    /// The names of the table's constraints, constraint triggers included.
    fn constraint_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError>;
    /// The names of the constraints of the table's schema.
    fn schema_constraint_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError>;
    fn partitions(&self) -> PartitionContext<'_>;
    fn rows(&self) -> ConstraintContext<'_>;
    fn validate_rows(&self, table: &str, name: &str, key: &ForeignKey) -> Result<(), SQLError> {
        crate::schema::validation::validate_foreign_key_rows(self.rows(), table, name, key)
    }
}

impl PartitionForeignKeyTables for ConstraintAlterContext<'_> {
    fn declared(&self, table: &str) -> Result<(Vec<ColumnDef>, TableConstraintSet), SQLError> {
        table_constraint_state(self, table)
    }
    fn bound_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, SQLError> {
        self.catalog
            .try_foreign_keys(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE foreign keys", error))
    }
    fn publish(
        &self,
        table: &str,
        columns: Vec<ColumnDef>,
        constraints: TableConstraintSet,
    ) -> Result<(), SQLError> {
        publish_constraint_state(self, table, columns, constraints)
    }
    fn constraint_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError> {
        self.publication.constraint_names().existing_names(table)
    }
    fn schema_constraint_names(&self, table: &str) -> Result<BTreeSet<String>, SQLError> {
        self.publication.constraint_names().automatic_names(table)
    }
    fn partitions(&self) -> PartitionContext<'_> {
        self.rows.partitions
    }
    fn rows(&self) -> ConstraintContext<'_> {
        self.rows
    }
    fn validate_rows(&self, table: &str, name: &str, key: &ForeignKey) -> Result<(), SQLError> {
        if self.deferred_rows.is_deferred() {
            self.pending_foreign_keys.retain(table, name, key);
            return Ok(());
        }
        crate::schema::validation::validate_foreign_key_rows(self.rows, table, name, key)
    }
}

fn family(foreign_key: &ForeignKey) -> Result<[u8; 16], SQLError> {
    foreign_key
        .object_id
        .ok_or_else(|| SQLError::Internal("materialized foreign key has no object identity".into()))
}

fn is_leaf(tables: &dyn PartitionForeignKeyTables, table: &str) -> Result<bool, SQLError> {
    Ok(tables
        .partitions()
        .catalog
        .try_table_hierarchy(table)
        .map_err(SQLError::Internal)?
        .partition_spec
        .is_none())
}

/// The families of attached foreign keys, which the copies on the attaching partition's own partitions follow, and the partition foreign keys that need their rows validated.
#[derive(Default)]
pub struct PartitionForeignKeyInheritance {
    joined: BTreeMap<[u8; 16], [u8; 16]>,
    validations: Vec<PartitionForeignKeyValidation>,
    /// Whether the inheritance repairs a catalog that holds partitions without their copies: a foreign key that differs from the parent's in enforceability alone receives a copy beside it instead of failing, and copies are not valid, as no validation reads their rows.
    repairing: bool,
}

/// The copies a partition received and whether its foreign keys changed.
pub struct InheritedForeignKeys {
    pub copies: Vec<ForeignKey>,
    pub changed: bool,
}

/// A partition's foreign key of a family whose rows must be validated, and whether the validation marks it validated.
struct PartitionForeignKeyValidation {
    table: String,
    family: [u8; 16],
    mark: bool,
}

impl PartitionForeignKeyInheritance {
    /// Give the declaration of `table`, a partition of `parent`, each of `parent_keys`, which `parent` holds: the copies of foreign keys an ancestor attached follow them, the partition attaches its first equivalent foreign key that copies none of its parent's, or it receives a copy. Records the rows to validate: a copy's rows when its foreign key is enforced, as `CloneFkReferencing` validates them whether the parent's foreign key is valid or not, and an attached foreign key's rows when it is not valid but the parent's is.
    pub fn inherit(
        &mut self,
        tables: &dyn PartitionForeignKeyTables,
        table: &str,
        parent: &str,
        parent_keys: &[ForeignKey],
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
    ) -> Result<InheritedForeignKeys, SQLError> {
        let mut changed = rejoin_foreign_key_families(columns, constraints, &self.joined);
        let (parent_columns, parent_constraints) = tables.declared(parent)?;
        let parent_families = declared_foreign_key_families(&parent_columns, &parent_constraints);
        let mut candidates = tables
            .bound_foreign_keys(table)?
            .into_iter()
            .map(|mut candidate| {
                if let Some(target) = candidate.object_id.and_then(|id| self.joined.get(&id)) {
                    candidate.object_id = Some(*target);
                }
                candidate
            })
            .filter(|candidate| {
                candidate
                    .object_id
                    .is_none_or(|object_id| !parent_families.contains(&object_id))
            })
            .collect::<Vec<_>>();
        let mut used = tables.constraint_names(table)?;
        let mut schema = tables.schema_constraint_names(table)?;
        let mut copies = Vec::new();
        for key in parent_keys {
            let key_family = family(key)?;
            if DeclaredForeignKey::by_family(columns, constraints, key_family).is_some() {
                continue;
            }
            let attached = match attachable_foreign_key(table, key, &candidates) {
                Ok(attached) => attached,
                Err(_) if self.repairing => None,
                Err(error) => return Err(error),
            };
            if let Some(candidate) = attached {
                let identity = candidate.catalog_identity.ok_or_else(|| {
                    SQLError::Internal("attached foreign key has no catalog identity".into())
                })?;
                let location =
                    DeclaredForeignKey::by_catalog_identity(columns, constraints, identity)
                        .ok_or_else(|| {
                            SQLError::Internal("attached foreign key disappeared".into())
                        })?;
                self.joined.insert(family(candidate)?, key_family);
                if key.enforced && key.validated && !candidate.validated {
                    self.validations.push(PartitionForeignKeyValidation {
                        table: table.to_string(),
                        family: key_family,
                        mark: true,
                    });
                }
                location.set_family(columns, constraints, key_family);
                candidates.retain(|candidate| candidate.catalog_identity != Some(identity));
                changed = true;
            } else {
                let mut copy = partition_foreign_key_copy(key, &used, &mut schema)?;
                if self.repairing {
                    copy.validated = false;
                }
                used.extend(copy.name.clone());
                schema.extend(copy.name.clone());
                if key.enforced {
                    self.validations.push(PartitionForeignKeyValidation {
                        table: table.to_string(),
                        family: key_family,
                        mark: false,
                    });
                }
                constraints.foreign_keys.push(copy.clone());
                copies.push(copy);
                changed = true;
            }
        }
        Ok(InheritedForeignKeys { copies, changed })
    }

    /// Validate the recorded partition foreign keys once every partition holds its foreign keys: a leaf partition's rows directly, and a partitioned partition's through the copies on its own partitions that are not yet validated.
    pub fn validate(self, tables: &dyn PartitionForeignKeyTables) -> Result<(), SQLError> {
        for validation in self.validations {
            if is_leaf(tables, &validation.table)? {
                validate_rows(tables, &validation.table, validation.family)?;
            } else if validation.mark {
                validate_partition_foreign_keys(tables, &validation.table, validation.family)?;
            }
            if validation.mark {
                let (mut columns, mut constraints) = tables.declared(&validation.table)?;
                let location =
                    DeclaredForeignKey::by_family(&columns, &constraints, validation.family)
                        .ok_or_else(|| {
                            SQLError::Internal("attached foreign key disappeared".into())
                        })?;
                location.set_validated(&mut columns, &mut constraints, true);
                tables.publish(&validation.table, columns, constraints)?;
            }
        }
        Ok(())
    }
}

fn validate_rows(
    tables: &dyn PartitionForeignKeyTables,
    table: &str,
    key_family: [u8; 16],
) -> Result<(), SQLError> {
    let key = tables
        .bound_foreign_keys(table)?
        .into_iter()
        .find(|candidate| candidate.object_id == Some(key_family))
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "partition `{table}` holds no copy of its parent's foreign key"
            ))
        })?;
    tables.validate_rows(table, key.name.as_deref().unwrap_or_default(), &key)
}

/// Give every partition below `table` the foreign key `key` that `table` now holds, parents before their partitions in partition order. The validation of `key` validates the copies and attached foreign keys that are not valid, as `ADD FOREIGN KEY` validates the rows of every leaf partition unless the foreign key is `NOT VALID`.
pub fn inherit_partition_foreign_key(
    tables: &dyn PartitionForeignKeyTables,
    table: &str,
    key: &ForeignKey,
) -> Result<(), SQLError> {
    let mut inheritance = PartitionForeignKeyInheritance::default();
    for node in uqa_sql::semantics::partition::partition_tree(&tables.partitions(), table, false)? {
        let (mut columns, mut constraints) = tables.declared(&node.table)?;
        let inherited = inheritance.inherit(
            tables,
            &node.table,
            &node.parent,
            std::slice::from_ref(key),
            &mut columns,
            &mut constraints,
        )?;
        if inherited.changed {
            tables.publish(&node.table, columns, constraints)?;
        }
    }
    Ok(())
}

/// Validate the copies of the foreign key family `key_family` on the partitions below `table`, scanning the rows of each leaf partition whose copy is not validated and marking every such copy validated. A validated copy implies validated copies below it, whose partitions are skipped, as `PostgreSQL`'s `QueueFKConstraintValidation` skips them.
pub fn validate_partition_foreign_keys(
    tables: &dyn PartitionForeignKeyTables,
    table: &str,
    key_family: [u8; 16],
) -> Result<(), SQLError> {
    let mut skipped = BTreeSet::new();
    for node in uqa_sql::semantics::partition::partition_tree(&tables.partitions(), table, false)? {
        if skipped.contains(&node.parent) {
            skipped.insert(node.table);
            continue;
        }
        let (mut columns, mut constraints) = tables.declared(&node.table)?;
        let location = DeclaredForeignKey::by_family(&columns, &constraints, key_family)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "partition `{}` holds no copy of its parent's foreign key",
                    node.table
                ))
            })?;
        if location.validated(&columns, &constraints) {
            skipped.insert(node.table);
            continue;
        }
        if is_leaf(tables, &node.table)? {
            validate_rows(tables, &node.table, key_family)?;
        }
        location.set_validated(&mut columns, &mut constraints, true);
        tables.publish(&node.table, columns, constraints)?;
    }
    Ok(())
}

fn declared_families(
    context: &ConstraintAlterContext<'_>,
    table: &str,
) -> Result<BTreeSet<[u8; 16]>, SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    Ok(declared_foreign_key_families(&columns, &constraints))
}

fn table_hierarchy(
    context: &ConstraintAlterContext<'_>,
    table: &str,
) -> Result<uqa_sql::ast::TableHierarchy, SQLError> {
    context
        .relations
        .table_hierarchy(table)
        .map_err(|error| ddl_storage_error("partition foreign key repair", error))
}

fn table_names(context: &ConstraintAlterContext<'_>) -> Result<Vec<String>, SQLError> {
    context
        .relations
        .table_names()
        .map_err(|error| ddl_storage_error("partition foreign key repair", error))
}

/// Whether a partition holds no copy of a foreign key its partitioned parent holds, a state that releases which did not recurse `ADD FOREIGN KEY` into existing partitions, or that kept an attached partition's equivalent foreign key apart from its parent's, left behind.
pub fn partition_foreign_keys_need_repair(
    context: &ConstraintAlterContext<'_>,
) -> Result<bool, SQLError> {
    for table in table_names(context)? {
        let hierarchy = table_hierarchy(context, &table)?;
        let Some(parent) = hierarchy
            .parents
            .first()
            .filter(|_| hierarchy.is_partition())
        else {
            continue;
        };
        let held = declared_families(context, &table)?;
        if declared_families(context, parent)?
            .iter()
            .any(|object_id| !held.contains(object_id))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Give every partition the copies of its partitioned ancestors' foreign keys that it lacks, attaching an equivalent foreign key of its own where one exists, as `ADD FOREIGN KEY` and `ATTACH PARTITION` now give them. No validation reads the rows of a repaired partition, so its copy, and every foreign key above an invalid one, is not valid until `VALIDATE CONSTRAINT` validates it.
pub fn repair_partition_foreign_keys(context: &ConstraintAlterContext<'_>) -> Result<(), SQLError> {
    for table in table_names(context)? {
        let hierarchy = table_hierarchy(context, &table)?;
        if hierarchy.partition_spec.is_none() {
            continue;
        }
        let inherited = match hierarchy
            .parents
            .first()
            .filter(|_| hierarchy.is_partition())
        {
            Some(parent) => declared_families(context, parent)?,
            None => BTreeSet::new(),
        };
        for key in context
            .bound_foreign_keys(&table)?
            .into_iter()
            .filter(|key| {
                key.object_id
                    .is_some_and(|object_id| !inherited.contains(&object_id))
            })
        {
            repair_family(context, &table, &key)?;
        }
    }
    Ok(())
}

fn repair_family(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key: &ForeignKey,
) -> Result<(), SQLError> {
    let key_family = family(key)?;
    let tree =
        uqa_sql::semantics::partition::partition_tree(&context.rows.partitions, table, false)?;
    let mut inheritance = PartitionForeignKeyInheritance {
        repairing: true,
        ..PartitionForeignKeyInheritance::default()
    };
    let mut repaired = false;
    for node in &tree {
        let (mut columns, mut constraints) = table_constraint_state(context, &node.table)?;
        let inherited = inheritance.inherit(
            context,
            &node.table,
            &node.parent,
            std::slice::from_ref(key),
            &mut columns,
            &mut constraints,
        )?;
        if inherited.changed {
            repaired = true;
            publish_constraint_state(context, &node.table, columns, constraints)?;
        }
    }
    if !repaired {
        return Ok(());
    }
    // A valid foreign key implies valid foreign keys below it.
    let mut valid = BTreeMap::<String, bool>::new();
    for node in tree.iter().rev() {
        invalidate_above_invalid(context, &node.table, key_family, &tree, &mut valid)?;
    }
    invalidate_above_invalid(context, table, key_family, &tree, &mut valid)
}

fn invalidate_above_invalid(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key_family: [u8; 16],
    tree: &[uqa_sql::semantics::partition::PartitionTreeNode],
    valid: &mut BTreeMap<String, bool>,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    let location = DeclaredForeignKey::by_family(&columns, &constraints, key_family)
        .ok_or_else(|| SQLError::Internal(format!("`{table}` lost its foreign key copy")))?;
    let children_valid = tree
        .iter()
        .filter(|node| node.parent == table)
        .all(|node| valid.get(&node.table).copied().unwrap_or(true));
    let validated = location.validated(&columns, &constraints);
    valid.insert(table.to_string(), validated && children_valid);
    if validated && !children_valid {
        location.set_validated(&mut columns, &mut constraints, false);
        publish_constraint_state(context, table, columns, constraints)?;
    }
    Ok(())
}

/// The foreign key without a parent that the copy of family `key_family` on `table` derives from: the one the farthest partitioned ancestor holding the family declares, or `None` when `table`'s foreign key is not a copy.
pub fn foreign_key_ancestor(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key_family: [u8; 16],
) -> Result<Option<uqa_sql::schema::constraint_changes::ConstraintAncestor>, SQLError> {
    let mut ancestor = None;
    let mut current = table.to_string();
    loop {
        let hierarchy = context
            .rows
            .partitions
            .catalog
            .try_table_hierarchy(&current)
            .map_err(SQLError::Internal)?;
        let Some(parent) = hierarchy
            .parents
            .first()
            .filter(|_| hierarchy.is_partition())
            .cloned()
        else {
            break;
        };
        let (columns, constraints) = table_constraint_state(context, &parent)?;
        let Some(key) = DeclaredForeignKey::by_family(&columns, &constraints, key_family)
            .and_then(|location| location.foreign_key(&columns, &constraints))
        else {
            break;
        };
        ancestor = Some(uqa_sql::schema::constraint_changes::ConstraintAncestor {
            name: key.name.unwrap_or_default(),
            table: parent.clone(),
        });
        current = parent;
    }
    Ok(ancestor)
}

/// Apply an `ALTER CONSTRAINT` enforceability and deferrability change of a partitioned table's foreign key to its copies on the partitions below it, as `PostgreSQL` alters every constraint deriving from it. A copy that becomes enforced is validated with the foreign key.
pub fn alter_partition_foreign_keys(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key_family: [u8; 16],
    enforceability: Option<bool>,
    deferrability: Option<(bool, bool)>,
) -> Result<(), SQLError> {
    for node in
        uqa_sql::semantics::partition::partition_tree(&context.rows.partitions, table, false)?
    {
        let (mut columns, mut constraints) = table_constraint_state(context, &node.table)?;
        let location = DeclaredForeignKey::by_family(&columns, &constraints, key_family)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "partition `{}` holds no copy of its parent's foreign key",
                    node.table
                ))
            })?;
        location.alter(
            &mut columns,
            &mut constraints,
            enforceability,
            deferrability,
        );
        publish_constraint_state(context, &node.table, columns, constraints)?;
    }
    Ok(())
}
