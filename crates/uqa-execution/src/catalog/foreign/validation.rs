//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted declarations share version, identity and option membership predicates.

use super::{reference, PersistedForeignTableSchema, FOREIGN_TABLE_SCHEMA_VERSION};
use uqa_core::memory::{OwnedSet, ProductionControl};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{StorageBackendError, StorageBackendResult};

impl PersistedForeignTableSchema {
    pub(super) fn legacy(columns: Vec<ColumnDef>) -> Self {
        Self {
            version: 1,
            object_id: [0; 16],
            catalog_oids: None,
            row_type_array_name: None,
            server_reference: None,
            option_order: None,
            columns,
            dropped_attributes: Vec::new(),
            checks: Vec::new(),
        }
    }

    pub(super) fn validate(&self, name: &str) -> StorageBackendResult<bool> {
        if !matches!(self.version, 1 | 2 | FOREIGN_TABLE_SCHEMA_VERSION) {
            return Err(StorageBackendError::Other(format!(
                "foreign table `{name}` has unsupported schema version {}",
                self.version
            )));
        }
        reference::validate_schema_reference(name, self.version, self.server_reference)?;
        if self.catalog_oids.is_some_and(|oids| {
            !oids.is_valid_for(
                uqa_sql::catalog::relation_oids::RelationOidKind::ForeignTable,
                &self.object_id,
            )
        }) {
            return Err(StorageBackendError::Other(format!(
                "foreign table `{name}` records invalid catalog OIDs"
            )));
        }
        Ok(self.version != FOREIGN_TABLE_SCHEMA_VERSION)
    }
}

pub(super) fn option_order(
    table: &str,
    order: &[String],
    count: usize,
    contains: impl Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<()> {
    let mut memory = control.empty_reservation();
    let mut names = OwnedSet::new();
    for name in order {
        control.check()?;
        if !names.contains(name.as_str()) {
            memory = control.combine(memory, control.reserve(OwnedSet::<&str>::entry_bytes())?);
            names.insert(name.as_str());
        }
    }
    let mut valid = names.len() == order.len() && names.len() == count;
    if valid {
        for name in &names {
            control.check()?;
            if !contains(name) {
                valid = false;
                break;
            }
        }
    }
    if valid {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "foreign table `{table}` option order disagrees with stored options"
        )))
    }
}
