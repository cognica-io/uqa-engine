//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native foreign generations publish matching definitions and authority through one boundary.

use super::StoredForeignTable;
use std::{iter::Peekable, sync::Arc};
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap,
        RetainedMapBidirectionalIter, RetainedMapSnapshot,
    },
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::security::{
        foreign_tables::{CapturedForeignTableSecurities, ForeignTableSecurities},
        BoundTableSecurity,
    },
    SQLError,
};

mod image;

#[derive(Clone)]
pub struct ForeignTables {
    durable: RetainedMap<RelationIdentity, Budgeted<StoredForeignTable>>,
    temporary: RetainedMap<RelationIdentity, Budgeted<StoredForeignTable>>,
}

impl Default for ForeignTables {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl ForeignTables {
    fn with_budget(memory: &MemoryBudget) -> Self {
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
    pub fn get(&self, relation: &RelationIdentity) -> Option<&StoredForeignTable> {
        self.durable
            .get(relation)
            .or_else(|| self.temporary.get(relation))
            .map(|value| &**value)
    }
    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.get(relation).is_some()
    }
    pub fn iter(&self) -> ForeignTableIter<'_> {
        ForeignTableIter {
            durable: self.durable.iter_bidirectional().peekable(),
            temporary: self.temporary.iter_bidirectional().peekable(),
        }
    }
    pub fn keys(&self) -> impl Iterator<Item = &RelationIdentity> {
        self.iter().map(|(key, _)| key)
    }
    pub fn values(&self) -> impl Iterator<Item = &StoredForeignTable> {
        self.iter().map(|(_, value)| value)
    }

    fn insert(
        &mut self,
        relation: RelationIdentity,
        table: StoredForeignTable,
    ) -> Result<(), SQLError> {
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(self.budget(), &cancellation, &cancellation);
        let (table, children) = super::capture::native_table(&table, &control)?;
        let table = table.into_budgeted().unwrap_or_else(|_| {
            unreachable!("native foreign definitions use admitted construction")
        });
        let bytes = relation
            .schema
            .capacity()
            .checked_add(relation.name.capacity())
            .and_then(|bytes| bytes.checked_add(table.reserved_bytes()))
            .and_then(|bytes| bytes.checked_add(children))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut durable = self.durable.clone();
        let mut temporary = self.temporary.clone();
        if table.persistence == RelationPersistence::Temporary {
            durable.remove(&relation)?;
            temporary.insert(relation, table, bytes)?;
        } else {
            temporary.remove(&relation)?;
            durable.insert(relation, table, bytes)?;
        }
        self.durable = durable;
        self.temporary = temporary;
        Ok(())
    }

    fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        let mut next = self.clone();
        let durable = next.durable.remove(relation)?;
        let temporary = next.temporary.remove(relation)?;
        *self = next;
        Ok(durable || temporary)
    }

    fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedForeignTables>, SQLError> {
        control.check()?;
        let budget = control.budget().ok_or_else(|| {
            SQLError::Internal("foreign capture requires an admitted control".into())
        })?;
        let (root, memory) = self.durable.capture(budget)?.into_parts();
        Ok(control.finish(CapturedForeignTables { root }, Some(memory))?)
    }
}

impl std::fmt::Debug for ForeignTables {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl std::ops::Index<&RelationIdentity> for ForeignTables {
    type Output = StoredForeignTable;
    fn index(&self, relation: &RelationIdentity) -> &Self::Output {
        self.get(relation).expect("foreign definition exists")
    }
}

pub struct ForeignTableIter<'a> {
    durable:
        Peekable<RetainedMapBidirectionalIter<'a, RelationIdentity, Budgeted<StoredForeignTable>>>,
    temporary:
        Peekable<RetainedMapBidirectionalIter<'a, RelationIdentity, Budgeted<StoredForeignTable>>>,
}

impl<'a> Iterator for ForeignTableIter<'a> {
    type Item = (&'a RelationIdentity, &'a StoredForeignTable);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = match (self.durable.peek(), self.temporary.peek()) {
            (Some((left, _)), Some((right, _))) if left < right => self.durable.next(),
            (_, Some(_)) => self.temporary.next(),
            (Some(_), None) => self.durable.next(),
            (None, None) => None,
        };
        entry.map(|(relation, value)| (relation, &**value))
    }
}

impl<'a> IntoIterator for &'a ForeignTables {
    type Item = (&'a RelationIdentity, &'a StoredForeignTable);
    type IntoIter = ForeignTableIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Debug)]
pub struct ForeignCatalog {
    tables: Arc<ForeignTables>,
    security: Arc<ForeignTableSecurities>,
}

impl Default for ForeignCatalog {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl ForeignCatalog {
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            tables: Arc::new(ForeignTables::with_budget(memory)),
            security: Arc::new(ForeignTableSecurities::with_budget(memory)),
        }
    }
    pub fn tables(&self) -> &ForeignTables {
        &self.tables
    }
    pub fn security(&self) -> &ForeignTableSecurities {
        &self.security
    }
    pub fn tables_snapshot(&self) -> Arc<ForeignTables> {
        self.tables.clone()
    }
    pub fn security_snapshot(&self) -> Arc<ForeignTableSecurities> {
        self.security.clone()
    }
    pub fn budget(&self) -> &MemoryBudget {
        self.tables.budget()
    }

    /// Build both immutable successors before exposing either root.
    pub fn insert(
        &mut self,
        relation: RelationIdentity,
        table: StoredForeignTable,
        security: BoundTableSecurity,
    ) -> Result<(), SQLError> {
        let mut next = self.clone();
        Arc::make_mut(&mut next.security).insert(relation.clone(), table.persistence, security)?;
        Arc::make_mut(&mut next.tables).insert(relation, table)?;
        *self = next;
        Ok(())
    }
    pub fn replace_table(
        &mut self,
        relation: RelationIdentity,
        table: StoredForeignTable,
    ) -> Result<(), SQLError> {
        let security = self
            .security
            .get(&relation)
            .cloned()
            .ok_or_else(|| SQLError::Internal("foreign replacement has no relation".into()))?;
        self.insert(relation, table, security)
    }
    pub fn replace_security(
        &mut self,
        relation: &RelationIdentity,
        security: BoundTableSecurity,
    ) -> Result<(), SQLError> {
        let table = self
            .tables
            .get(relation)
            .ok_or_else(|| SQLError::Internal("foreign authority has no relation".into()))?;
        let mut next = (*self.security).clone();
        next.insert(relation.clone(), table.persistence, security)?;
        self.security = Arc::new(next);
        Ok(())
    }
    pub fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        let mut next = self.clone();
        let removed = Arc::make_mut(&mut next.tables).remove(relation)?;
        Arc::make_mut(&mut next.security).remove(relation)?;
        *self = next;
        Ok(removed)
    }
    pub fn retain(
        &mut self,
        mut keep: impl FnMut(&RelationIdentity, &StoredForeignTable) -> bool,
    ) -> Result<(), SQLError> {
        let mut next = self.clone();
        for (relation, table) in self.tables.iter() {
            if !keep(relation, table) {
                next.remove(relation)?;
            }
        }
        *self = next;
        Ok(())
    }
    pub fn with_table_updates(
        &self,
        updates: &[(RelationIdentity, StoredForeignTable)],
    ) -> Result<Self, SQLError> {
        let mut next = self.clone();
        for (relation, table) in updates {
            next.replace_table(relation.clone(), table.clone())?;
        }
        Ok(next)
    }
    /// The paired durable roots already have identical membership and persistence.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedForeignCatalog>, SQLError> {
        let (tables, table_memory) = self.tables.capture(control)?.into_parts();
        let (security, security_memory) = self.security.capture(control)?.into_parts();
        Ok(control.finish(
            CapturedForeignCatalog { tables, security },
            control.combine(table_memory, security_memory),
        )?)
    }
}

pub struct CapturedForeignTables {
    root: RetainedMapSnapshot<RelationIdentity, Budgeted<StoredForeignTable>>,
}
impl CapturedForeignTables {
    pub fn len(&self) -> usize {
        self.root.len()
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&StoredForeignTable> {
        self.root.get(relation).map(|value| &**value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RelationIdentity, &StoredForeignTable)> {
        self.root.iter().map(|(key, value)| (key, &**value))
    }
}

pub struct CapturedForeignCatalog {
    pub tables: CapturedForeignTables,
    pub security: CapturedForeignTableSecurities,
}

#[cfg(test)]
mod tests;
