//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete sequence records, bound authority and retained OID metadata publish together.

use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl, RetainedMap},
    RelationIdentity,
};
use uqa_sql::{
    catalog::security::{
        sequence_registry::{CapturedSequenceSecurities, SequenceSecurities},
        BoundSequenceSecurity,
    },
    SQLError,
};

pub mod publication;
mod records;
use records::SequenceRecords;
pub use records::{
    CapturedSequenceRecords, PreparedSequencePosition, SequenceRecord, SequenceRecordIter,
};

pub type SequenceCatalogRead<'a> = Box<dyn std::ops::Deref<Target = SequenceCatalog> + 'a>;
pub type SequenceCatalogWrite<'a> = Box<dyn std::ops::DerefMut<Target = SequenceCatalog> + 'a>;

#[derive(Clone)]
pub struct SequenceCatalog {
    records: SequenceRecords,
    security: SequenceSecurities,
    catalog_oids: RetainedMap<[u8; 16], u32>,
    durable_objects: RetainedMap<[u8; 16], [u8; 16]>,
    temporary_objects: RetainedMap<[u8; 16], [u8; 16]>,
}

impl Default for SequenceCatalog {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl SequenceCatalog {
    pub fn try_from_iter(
        entries: impl IntoIterator<Item = (RelationIdentity, SequenceRecord, BoundSequenceSecurity)>,
    ) -> Result<Self, SQLError> {
        let mut catalog = Self::default();
        for (relation, record, security) in entries {
            catalog.insert(relation, record, security)?;
        }
        Ok(catalog)
    }
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            records: SequenceRecords::new(memory),
            security: SequenceSecurities::with_budget(memory),
            catalog_oids: RetainedMap::new(memory),
            durable_objects: RetainedMap::new(memory),
            temporary_objects: RetainedMap::new(memory),
        }
    }
    /// Complete native generation identity survives abandoned preparations and a copied outer owner, while any published record, authority or OID change invalidates it.
    pub fn shares_generation(&self, other: &Self) -> bool {
        self.records.shares_roots(&other.records)
            && self.security.shares_roots(&other.security)
            && self.catalog_oids.shares_root(&other.catalog_oids)
            && self.durable_objects.shares_root(&other.durable_objects)
            && self.temporary_objects.shares_root(&other.temporary_objects)
    }
    pub fn budget(&self) -> &MemoryBudget {
        self.records.budget()
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&SequenceRecord> {
        self.records.get(relation)
    }
    pub fn contains_key(&self, relation: &RelationIdentity) -> bool {
        self.get(relation).is_some()
    }
    pub fn iter(&self) -> SequenceRecordIter<'_> {
        self.records.iter()
    }
    pub fn keys(&self) -> impl Iterator<Item = &RelationIdentity> {
        self.iter().map(|(relation, _)| relation)
    }
    pub fn state(&self, relation: &RelationIdentity) -> Option<&super::SequenceState> {
        self.get(relation).map(|record| &record.state)
    }
    pub fn states(&self) -> impl Iterator<Item = (&RelationIdentity, &super::SequenceState)> {
        self.iter()
            .map(|(relation, record)| (relation, &record.state))
    }
    pub fn values(&self) -> impl Iterator<Item = &super::SequenceState> {
        self.states().map(|(_, state)| state)
    }
    pub fn persistences(
        &self,
    ) -> impl Iterator<Item = (&RelationIdentity, &uqa_sql::ast::RelationPersistence)> {
        self.iter()
            .map(|(relation, record)| (relation, &record.persistence))
    }
    pub fn object_id(&self, relation: &RelationIdentity) -> Option<&[u8; 16]> {
        self.get(relation).map(|record| &record.object_id)
    }
    pub fn object_ids(&self) -> impl Iterator<Item = (&RelationIdentity, &[u8; 16])> {
        self.iter()
            .map(|(relation, record)| (relation, &record.object_id))
    }
    pub fn persistence(
        &self,
        relation: &RelationIdentity,
    ) -> Option<uqa_sql::ast::RelationPersistence> {
        self.get(relation).map(|record| record.persistence)
    }
    pub fn security(&self) -> &SequenceSecurities {
        &self.security
    }
    pub fn catalog_oids(&self) -> &RetainedMap<[u8; 16], u32> {
        &self.catalog_oids
    }
    pub fn catalog_oid(&self, object_id: &[u8; 16]) -> i64 {
        super::catalog_oids::sequence_catalog_oid(&self.catalog_oids, object_id)
    }
    pub fn contains_durable_definition(&self, object_id: &[u8; 16], definition: &[u8; 16]) -> bool {
        self.durable_objects.get(object_id) == Some(definition)
    }

    /// Retain temporary complete records while preparing a replacement from stored metadata. Historical OID entries remain available to the loader until it selects its recorded metadata.
    pub(super) fn temporary_only(&self) -> Self {
        Self {
            records: self.records.temporary_only(),
            security: self.security.temporary_only(),
            catalog_oids: self.catalog_oids.clone(),
            durable_objects: RetainedMap::new(self.budget()),
            temporary_objects: self.temporary_objects.clone(),
        }
    }

    /// Select complete private records without discarding historical OID metadata still owned by the selected catalog view.
    pub(super) fn remove_selected(&mut self, relation: &RelationIdentity) -> Result<(), SQLError> {
        let mut next = self.clone();
        if let Some(record) = self.get(relation) {
            next.durable_objects.remove(&record.object_id)?;
            next.temporary_objects.remove(&record.object_id)?;
        }
        next.records.remove(relation)?;
        next.security.remove(relation)?;
        *self = next;
        Ok(())
    }

    pub(super) fn replace_catalog_oids(
        &mut self,
        entries: impl IntoIterator<Item = ([u8; 16], u32)>,
    ) -> Result<(), SQLError> {
        let mut next = self.clone();
        next.catalog_oids = RetainedMap::new(self.budget());
        for (object_id, oid) in entries {
            next.catalog_oids.insert(object_id, oid, 0)?;
        }
        for (relation, record) in self.iter() {
            let oid = next.catalog_oids.get(&record.object_id).copied();
            if record.catalog_oid != oid {
                next.records.insert(
                    relation,
                    SequenceRecord {
                        catalog_oid: oid,
                        ..*record
                    },
                )?;
            }
        }
        *self = next;
        Ok(())
    }

    pub fn rename(
        &mut self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError> {
        let record = self.get(source).copied().ok_or_else(|| {
            SQLError::Internal(format!(
                "sequence registry entry `{}` disappeared during rename",
                source.qualified_name()
            ))
        })?;
        if self.contains_key(target) {
            return Err(SQLError::Internal(format!(
                "sequence registry target `{}` appeared during rename",
                target.qualified_name()
            )));
        }
        let security = self.security[source].clone();
        let mut next = self.clone();
        next.remove_selected(source)?;
        next.insert(target.clone(), record, security)?;
        *self = next;
        Ok(())
    }

    /// Admit complete matching successors before exposing a record, authority or OID change.
    pub fn insert(
        &mut self,
        relation: RelationIdentity,
        mut record: SequenceRecord,
        security: BoundSequenceSecurity,
    ) -> Result<(), SQLError> {
        let previous = self.get(&relation);
        if previous.is_none_or(|row| row.object_id != record.object_id)
            && (self.durable_objects.get(&record.object_id).is_some()
                || self.temporary_objects.get(&record.object_id).is_some())
        {
            return Err(SQLError::Internal(
                "sequence object identity already belongs to another record".into(),
            ));
        }
        let mut next = self.clone();
        if let Some(previous) = previous {
            next.durable_objects.remove(&previous.object_id)?;
            next.temporary_objects.remove(&previous.object_id)?;
        }
        let objects = if record.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            &mut next.temporary_objects
        } else {
            &mut next.durable_objects
        };
        objects.insert(record.object_id, record.state.definition_generation, 0)?;
        next.security
            .insert(relation.clone(), record.persistence, security)?;
        if let Some(oid) = record.catalog_oid {
            next.catalog_oids.insert(record.object_id, oid, 0)?;
        } else {
            record.catalog_oid = next.catalog_oids.get(&record.object_id).copied();
        }
        next.records.insert(&relation, record)?;
        *self = next;
        Ok(())
    }

    pub fn replace_state(
        &mut self,
        relation: &RelationIdentity,
        state: super::SequenceState,
    ) -> Result<(), SQLError> {
        let mut record = self
            .get(relation)
            .copied()
            .ok_or_else(|| SQLError::Internal("sequence replacement has no record".into()))?;
        record.state = state;
        let mut next = self.clone();
        next.records.insert(relation, record)?;
        let objects = if record.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            &mut next.temporary_objects
        } else {
            &mut next.durable_objects
        };
        objects.insert(record.object_id, state.definition_generation, 0)?;
        *self = next;
        Ok(())
    }

    pub fn replace_security(
        &mut self,
        relation: &RelationIdentity,
        security: BoundSequenceSecurity,
    ) -> Result<(), SQLError> {
        let persistence = self
            .get(relation)
            .ok_or_else(|| SQLError::Internal("sequence authority has no record".into()))?
            .persistence;
        self.security
            .insert(relation.clone(), persistence, security)
    }

    pub fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        let Some(record) = self.get(relation).copied() else {
            return Ok(false);
        };
        let mut next = self.clone();
        next.remove_selected(relation)?;
        next.catalog_oids.remove(&record.object_id)?;
        *self = next;
        Ok(true)
    }

    pub fn prepare_position(
        &mut self,
        relation: &RelationIdentity,
        object: [u8; 16],
        definition: [u8; 16],
    ) -> Result<Option<PreparedSequencePosition<'_>>, SQLError> {
        self.records.prepare_position(relation, object, definition)
    }

    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedSequenceCatalog>, SQLError> {
        let (records, record_memory) = self.records.capture(control)?.into_parts();
        let (security, security_memory) = self.security.capture(control)?.into_parts();
        Ok(control.finish(
            CapturedSequenceCatalog { records, security },
            control.combine(record_memory, security_memory),
        )?)
    }
}

pub struct CapturedSequenceCatalog {
    pub records: CapturedSequenceRecords,
    pub security: CapturedSequenceSecurities,
}

#[cfg(test)]
mod tests;
