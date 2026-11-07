//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native index rows retain exact encodings and their owning table's durable eligibility.

use std::ops::Bound;
use uqa_core::{
    catalog_index::{CatalogIndexRegistry, CatalogIndexRegistryIter},
    memory::{Budgeted, MemoryBudget, ProductionControl, RetainedMap, RetainedMapSnapshot},
    CancellationToken, RelationIdentity,
};
use uqa_sql::ast::RelationPersistence;
use uqa_storage::{
    read_control::StorageReadControl, CatalogIndexRow, StorageBackendError, StorageBackendResult,
};

mod image;

#[derive(Clone)]
pub struct CatalogIndexes {
    durable: RetainedMap<RelationIdentity, Budgeted<CatalogIndexRow>>,
    temporary: RetainedMap<RelationIdentity, Budgeted<CatalogIndexRow>>,
}

impl Default for CatalogIndexes {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl CatalogIndexes {
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            durable: RetainedMap::new(memory),
            temporary: RetainedMap::new(memory),
        }
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.durable.budget()
    }

    pub fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, relation: &RelationIdentity) -> Option<&CatalogIndexRow> {
        self.durable
            .get(relation)
            .or_else(|| self.temporary.get(relation))
            .map(|row| &**row)
    }

    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.get(relation).is_some()
    }

    pub fn is_durable(&self, relation: &RelationIdentity) -> bool {
        self.durable.get(relation).is_some()
    }

    pub fn without_table(&self, table: &str) -> StorageBackendResult<Self> {
        let mut candidate = self.clone();
        for (relation, row) in self.iter() {
            if row.table_name == table {
                candidate.remove(relation)?;
            }
        }
        Ok(candidate)
    }

    pub fn iter(&self) -> CatalogIndexRegistryIter<'_> {
        CatalogIndexRegistryIter::new(self)
    }

    pub fn first_from(
        &self,
        start: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        let entry = match (
            self.durable.first_from(start),
            self.temporary.first_from(start),
        ) {
            (Some(left), Some(right)) => Some(if left.0 < right.0 { left } else { right }),
            (left, right) => left.or(right),
        };
        entry.map(|(key, row)| (key, &**row))
    }

    pub fn last_before(
        &self,
        end: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        let entry = match (
            self.durable.last_before(end),
            self.temporary.last_before(end),
        ) {
            (Some(left), Some(right)) => Some(if left.0 > right.0 { left } else { right }),
            (left, right) => left.or(right),
        };
        entry.map(|(key, row)| (key, &**row))
    }

    pub fn keys(&self) -> impl Iterator<Item = &RelationIdentity> {
        self.iter().map(|(relation, _)| relation)
    }

    pub fn values(&self) -> impl Iterator<Item = &CatalogIndexRow> {
        self.iter().map(|(_, row)| row)
    }

    pub(crate) fn insert_in(
        &mut self,
        row: CatalogIndexRow,
        catalog: &crate::catalog::CatalogReadSnapshot,
    ) -> StorageBackendResult<()> {
        let relation = RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let table = catalog.tables.get(&relation).ok_or_else(|| {
            StorageBackendError::Other(format!("index table `{}` disappeared", row.table_name))
        })?;
        self.insert(row, table.persistence)
    }

    /// Admit the key, exact row payload and changed tree paths before publishing either partition.
    pub fn insert(
        &mut self,
        row: CatalogIndexRow,
        persistence: RelationPersistence,
    ) -> StorageBackendResult<()> {
        let token = CancellationToken::new();
        let control = ProductionControl::new(self.budget(), &token, &token);
        let (schema, schema_memory) = control.copy_text(&row.relation.schema)?.into_parts();
        let (name, name_memory) = control.copy_text(&row.relation.name)?.into_parts();
        let (row, row_memory) = uqa_storage::catalog::capture::index(&row, &control)?.into_parts();
        let row = control
            .finish(
                row,
                control.combine(control.combine(schema_memory, name_memory), row_memory),
            )?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native index construction retains its allowance"));
        let relation = RelationIdentity { schema, name };
        let bytes = row.reserved_bytes();
        let mut candidate = self.clone();
        if persistence == RelationPersistence::Temporary {
            candidate.durable.remove(&relation)?;
            candidate.temporary.insert(relation, row, bytes)?;
        } else {
            candidate.temporary.remove(&relation)?;
            candidate.durable.insert(relation, row, bytes)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Replace a selected row without changing the owning table's eligibility.
    pub fn replace(&mut self, row: CatalogIndexRow) -> StorageBackendResult<()> {
        let persistence = if self.temporary.get(&row.relation).is_some() {
            RelationPersistence::Temporary
        } else if self.durable.get(&row.relation).is_some() {
            RelationPersistence::Permanent
        } else {
            return Err(StorageBackendError::Other(
                "cannot replace an absent native index row".into(),
            ));
        };
        self.insert(row, persistence)
    }

    pub fn remove(&mut self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        let mut candidate = self.clone();
        let durable = candidate.durable.remove(relation)?;
        let temporary = candidate.temporary.remove(relation)?;
        *self = candidate;
        Ok(durable || temporary)
    }

    /// Rename a selected record while its table may still be published under the preceding name.
    pub fn rename(
        &mut self,
        previous: &RelationIdentity,
        row: CatalogIndexRow,
    ) -> StorageBackendResult<()> {
        let persistence = if self.temporary.get(previous).is_some() {
            RelationPersistence::Temporary
        } else if self.durable.get(previous).is_some() {
            RelationPersistence::Permanent
        } else {
            return Err(StorageBackendError::Other(
                "cannot rename an absent native index row".into(),
            ));
        };
        let mut candidate = self.clone();
        candidate.remove(previous)?;
        candidate.insert(row, persistence)?;
        *self = candidate;
        Ok(())
    }

    pub fn clear_durable(&mut self) {
        self.durable.clear();
    }

    /// Attach the session's existing temporary root to a fully admitted durable restoration.
    pub fn with_temporary_from(&self, current: &Self) -> StorageBackendResult<Self> {
        if !current.temporary.is_empty() && !self.budget().shares_allowance(current.budget()) {
            return Err(StorageBackendError::Other(
                "native index roots use different allowances".into(),
            ));
        }
        if self
            .durable
            .iter()
            .any(|(relation, _)| current.temporary.get(relation).is_some())
        {
            return Err(StorageBackendError::Other(
                "durable index conflicts with a temporary index".into(),
            ));
        }
        let mut candidate = self.clone();
        if !current.temporary.is_empty() {
            candidate.temporary = current.temporary.clone();
        }
        Ok(candidate)
    }

    /// Retain only the immutable durable root; cached admission never reads or copies a row.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedCatalogIndexes>> {
        control.check()?;
        let (rows, memory) = self.durable.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(CapturedCatalogIndexes { rows }, memory))
    }
}

impl std::fmt::Debug for CatalogIndexes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl std::ops::Index<&RelationIdentity> for CatalogIndexes {
    type Output = CatalogIndexRow;
    fn index(&self, relation: &RelationIdentity) -> &Self::Output {
        self.get(relation).expect("native index row exists")
    }
}

impl CatalogIndexRegistry for CatalogIndexes {
    fn len(&self) -> usize {
        self.len()
    }
    fn first_from(
        &self,
        start: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        self.first_from(start)
    }
    fn last_before(
        &self,
        end: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        self.last_before(end)
    }
}

impl<'a> IntoIterator for &'a CatalogIndexes {
    type Item = (&'a RelationIdentity, &'a CatalogIndexRow);
    type IntoIter = CatalogIndexRegistryIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct CapturedCatalogIndexes {
    rows: RetainedMapSnapshot<RelationIdentity, Budgeted<CatalogIndexRow>>,
}

impl CapturedCatalogIndexes {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&CatalogIndexRow> {
        self.rows.get(relation).map(|row| &**row)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RelationIdentity, &CatalogIndexRow)> {
        self.rows.iter().map(|(relation, row)| (relation, &**row))
    }
}

#[cfg(test)]
mod tests;
