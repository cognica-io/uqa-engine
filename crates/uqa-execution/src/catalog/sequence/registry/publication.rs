//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stage complete native successors before writing sequence metadata in the owning transaction.

use super::{SequenceCatalog, SequenceRecord};
use crate::storage_errors::storage_error;
use uqa_core::RelationIdentity;
use uqa_sql::{ast::RelationPersistence, catalog::security::BoundSequenceSecurity, SQLError};
use uqa_storage::CatalogFacade;

pub fn create(
    catalog: &mut SequenceCatalog,
    storage: Option<&dyn CatalogFacade>,
    name: &str,
    relation: &RelationIdentity,
    record: SequenceRecord,
    authority: BoundSequenceSecurity,
) -> Result<bool, SQLError> {
    let persistent = storage.filter(|_| record.persistence != RelationPersistence::Temporary);
    if persistent.is_none() && catalog.contains_key(relation) {
        return Ok(false);
    }
    let mut next = catalog.clone();
    next.insert(relation.clone(), record, authority.clone())?;
    if let Some(storage) = persistent {
        let row = super::super::sequence_row(
            name,
            record.object_id,
            record.state,
            record.persistence,
            &authority,
        )
        .map_err(|error| storage_error("build sequence catalog row", &error))?;
        if !storage
            .create_sequence_row(&row)
            .map_err(|error| storage_error("persist sequence catalog", &error))?
        {
            return Ok(false);
        }
        if let Some(oid) = record.catalog_oid {
            super::super::catalog_oids::record(storage, &record.object_id, oid)
                .map_err(|error| storage_error("persist sequence catalog OID", &error))?;
        }
    }
    *catalog = next;
    Ok(true)
}

pub fn replace(
    catalog: &mut SequenceCatalog,
    storage: Option<&dyn CatalogFacade>,
    name: &str,
    relation: &RelationIdentity,
    record: SequenceRecord,
) -> Result<(), SQLError> {
    let authority =
        catalog.security().get(relation).cloned().ok_or_else(|| {
            SQLError::Internal(format!("sequence `{name}` has no security metadata"))
        })?;
    let mut next = catalog.clone();
    next.insert(relation.clone(), record, authority.clone())?;
    if let Some(storage) = storage.filter(|_| record.persistence != RelationPersistence::Temporary)
    {
        let row = super::super::sequence_row(
            name,
            record.object_id,
            record.state,
            record.persistence,
            &authority,
        )
        .map_err(|error| storage_error("build sequence catalog row", &error))?;
        if !storage
            .replace_sequence_row(&row)
            .map_err(|error| storage_error("persist sequence catalog", &error))?
        {
            return Err(SQLError::Internal(format!(
                "sequence `{name}` disappeared during ALTER"
            )));
        }
    }
    *catalog = next;
    Ok(())
}

pub fn remove(
    catalog: &mut SequenceCatalog,
    storage: Option<&dyn CatalogFacade>,
    name: &str,
) -> Result<Option<SequenceRecord>, SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
    let record = catalog
        .get(&relation)
        .copied()
        .ok_or_else(|| SQLError::Internal(format!("Sequence `{name}` has no object identity")))?;
    let mut next = catalog.clone();
    next.remove(&relation)?;
    if let Some(storage) = storage.filter(|_| record.persistence != RelationPersistence::Temporary)
    {
        if !storage
            .drop_sequence_row(name)
            .map_err(|error| storage_error("persist sequence catalog", &error))?
        {
            return Ok(None);
        }
        super::super::catalog_oids::forget(storage, &record.object_id)
            .map_err(|error| storage_error("persist sequence catalog OID", &error))?;
    }
    *catalog = next;
    Ok(Some(record))
}

pub fn rename(
    catalog: &mut SequenceCatalog,
    storage: Option<&dyn CatalogFacade>,
    source: &RelationIdentity,
    target: &RelationIdentity,
) -> Result<(), SQLError> {
    let mut next = catalog.clone();
    next.rename(source, target)?;
    if let Some(storage) =
        storage.filter(|_| catalog.persistence(source) != Some(RelationPersistence::Temporary))
    {
        if !storage
            .rename_sequence_row(&source.qualified_name(), &target.qualified_name())
            .map_err(|error| storage_error("persist sequence rename", &error))?
        {
            return Err(SQLError::Internal(format!(
                "sequence `{}` disappeared during rename",
                source.qualified_name()
            )));
        }
    }
    *catalog = next;
    Ok(())
}
