//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish selected relation constraints through physical row keys in the definition transaction.

use crate::catalog::{context::CatalogContext, CatalogReadView, RelationNameResolution};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_core::{catalog_identity::CatalogObjectIdentity, RelationIdentity};
use uqa_sql::{schema::constraint_metadata::CatalogOidClass, SQLError};
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

use super::{constraint::ConstraintTuple, PhysicalTuple};

const PENDING: &str = "uqa.sql.catalog.constraint.pending:";
const MEMBERS: &str = "uqa.sql.catalog.constraint.members:";
const INITIALIZED: &str = "uqa.sql.catalog.constraint.format";

pub fn requires_initialization(catalog: &dyn CatalogFacade) -> StorageBackendResult<bool> {
    match catalog.get_metadata(INITIALIZED)?.as_deref() {
        None => Ok(true),
        Some("1") => Ok(false),
        Some(_) => Err(super::corrupt(
            "unsupported physical constraint catalog format",
        )),
    }
}

pub fn initialize(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: &PersistentStorageSession,
) -> Result<(), SQLError> {
    if !requires_initialization(session.catalog.as_ref()).map_err(error)? {
        return Ok(());
    }
    super::transaction::atomic(session, || {
        for (relation, table) in &catalog.snapshot().tables {
            if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                queue_relation(session.catalog.as_ref(), relation, table.object_id, true)?;
            }
        }
        for (relation, table) in &*catalog.snapshot().definitions.foreign_tables {
            if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                queue_relation(session.catalog.as_ref(), relation, table.object_id, true)?;
            }
        }
        for triggers in catalog.snapshot().definitions.triggers.values() {
            for trigger in triggers.values() {
                let relation = RelationIdentity::from_legacy_name(&trigger.definition.table)
                    .map_err(StorageBackendError::Other)?;
                if catalog
                    .snapshot()
                    .tables
                    .get(&relation)
                    .is_some_and(|table| {
                        table.persistence != uqa_sql::ast::RelationPersistence::Temporary
                    })
                {
                    queue_trigger(session.catalog.as_ref(), trigger)?;
                }
            }
        }
        for domain in catalog.domains() {
            queue_domain(session.catalog.as_ref(), domain, None)?;
        }
        publish_pending(context, catalog, resolution, session)
            .map_err(|error| StorageBackendError::backend("initialize constraint rows", error))?;
        session.catalog.set_metadata(INITIALIZED, "1")
    })
    .map_err(error)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationPublication {
    object: [u8; 16],
    relation: RelationIdentity,
    source: PublicationSource,
    replace: std::collections::BTreeSet<[u8; 16]>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum PublicationSource {
    Relation,
    Domain,
    Trigger,
}

fn key(prefix: &str, object: [u8; 16]) -> String {
    format!("{prefix}{:032x}", u128::from_be_bytes(object))
}

/// Record the selected definition, not a snapshot of unrelated relations. The private metadata root owns rollback and savepoint restoration of this pending publication.
pub fn queue_relation(
    catalog: &dyn CatalogFacade,
    relation: &RelationIdentity,
    object: [u8; 16],
    has_constraints: bool,
) -> StorageBackendResult<()> {
    if !has_constraints && catalog.get_metadata(&key(MEMBERS, object))?.is_none() {
        return Ok(());
    }
    queue(catalog, relation, object, PublicationSource::Relation, None)
}

pub fn queue_domain(
    catalog: &dyn CatalogFacade,
    domain: &uqa_sql::catalog::domain::StoredDomain,
    replace: Option<CatalogObjectIdentity>,
) -> StorageBackendResult<()> {
    queue(
        catalog,
        &domain.identity,
        domain.object_id,
        PublicationSource::Domain,
        replace,
    )
}

pub fn queue_trigger(
    catalog: &dyn CatalogFacade,
    trigger: &uqa_sql::catalog::events::StoredTrigger,
) -> StorageBackendResult<()> {
    if !trigger.definition.constraint {
        return Ok(());
    }
    let object = trigger
        .object_id
        .ok_or_else(|| super::corrupt("constraint trigger has no identity"))?;
    let relation = RelationIdentity::from_legacy_name(&trigger.definition.table)
        .map_err(StorageBackendError::Other)?;
    queue(catalog, &relation, object, PublicationSource::Trigger, None)
}

fn queue(
    catalog: &dyn CatalogFacade,
    relation: &RelationIdentity,
    object: [u8; 16],
    source: PublicationSource,
    replacement: Option<CatalogObjectIdentity>,
) -> StorageBackendResult<()> {
    let pending_key = key(PENDING, object);
    let mut replace = catalog
        .get_metadata(&pending_key)?
        .map(|json| super::decode::<RelationPublication>(&json).map(|entry| entry.replace))
        .transpose()?
        .unwrap_or_default();
    replace.extend(replacement.map(|identity| identity.object_id));
    catalog.set_metadata(
        &pending_key,
        &serde_json::to_string(&RelationPublication {
            object,
            relation: relation.clone(),
            source,
            replace,
        })?,
    )
}

pub fn has_pending(catalog: &dyn CatalogFacade) -> StorageBackendResult<bool> {
    let mut pending = false;
    catalog.visit_metadata_with_key_length(PENDING, PENDING.len() + 32, &mut |_, _| {
        pending = true;
        Ok(())
    })?;
    Ok(pending)
}

/// Finish a complete definition operation after its registry guards have been released. Provisional declarations never enqueue publication. The physical row/head/membership updates share the caller's paired transaction.
pub fn publish_pending(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: &PersistentStorageSession,
) -> Result<(), SQLError> {
    let mut pending = Vec::new();
    session
        .catalog
        .visit_metadata_with_key_length(PENDING, PENDING.len() + 32, &mut |_, json| {
            pending.push(super::decode::<RelationPublication>(json)?);
            Ok(())
        })
        .map_err(error)?;
    for publication in pending {
        publish_relation(context, catalog, resolution, session, &publication)?;
    }
    Ok(())
}

fn publish_relation(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    session: &PersistentStorageSession,
    publication: &RelationPublication,
) -> Result<(), SQLError> {
    super::transaction::atomic(session, || {
        let membership_key = key(MEMBERS, publication.object);
        let mut previous: BTreeMap<String, CatalogObjectIdentity> = session
            .catalog
            .get_metadata(&membership_key)?
            .map(|json| super::decode(&json))
            .transpose()?
            .unwrap_or_default();
        let mut next = BTreeMap::new();
        visit_selected(
            context,
            catalog,
            resolution,
            publication,
            &mut |identity, tuple| {
                publish_tuple(
                    session,
                    identity,
                    &tuple,
                    publication.replace.contains(&identity.object_id),
                )
                .map_err(error)?;
                let object = format!("{:032x}", u128::from_be_bytes(identity.object_id));
                previous.remove(&object);
                next.insert(object, identity);
                Ok(())
            },
        )
        .map_err(|error| StorageBackendError::backend("construct constraint tuple", error))?;
        for identity in previous.into_values() {
            let predecessor = super::current(session, CatalogOidClass::Constraint, identity)?
                .ok_or_else(|| super::corrupt("constraint membership has no current tuple"))?;
            super::remove(session, CatalogOidClass::Constraint, predecessor)?;
        }
        if next.is_empty() {
            session.catalog.delete_metadata(&membership_key)?;
        } else {
            session
                .catalog
                .set_metadata(&membership_key, &serde_json::to_string(&next)?)?;
        }
        session
            .catalog
            .delete_metadata(&key(PENDING, publication.object))
    })
    .map_err(error)
}

fn visit_selected(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    publication: &RelationPublication,
    visitor: &mut dyn FnMut(CatalogObjectIdentity, ConstraintTuple) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    match publication.source {
        PublicationSource::Relation => {
            if let Some(table) = catalog
                .snapshot()
                .tables
                .get(&publication.relation)
                .filter(|table| table.object_id == publication.object)
            {
                crate::catalog::projection::visit_table_constraint_tuples(
                    context,
                    catalog,
                    resolution,
                    &publication.relation,
                    table,
                    &crate::catalog::projection::CatalogRequest::default(),
                    visitor,
                )?;
            } else if let Some(table) = catalog
                .snapshot()
                .definitions
                .foreign_tables
                .get(&publication.relation)
                .filter(|table| table.object_id == publication.object)
            {
                crate::catalog::projection::visit_foreign_constraint_tuples(
                    context,
                    catalog,
                    resolution,
                    &publication.relation,
                    table,
                    &crate::catalog::projection::CatalogRequest::default(),
                    visitor,
                )?;
            }
        }
        PublicationSource::Trigger => {
            crate::catalog::projection::visit_trigger_constraint_tuples(
                catalog,
                resolution,
                publication.object,
                visitor,
            )?;
        }
        PublicationSource::Domain => {
            if let Some(domain) = catalog
                .snapshot()
                .definitions
                .domains
                .get(&publication.relation.qualified_name())
                .filter(|domain| domain.object_id == publication.object)
            {
                let _ = crate::catalog::projection::visit_domain_constraint_tuples(
                    Some(context),
                    catalog,
                    domain,
                    &crate::catalog::projection::CatalogRequest::default(),
                    &mut |identity, tuple| {
                        visitor(identity, tuple)?;
                        Ok(std::ops::ControlFlow::Continue(()))
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// Select an exact predecessor through its current head. Equality can omit representation-only persistence; an explicit replacement intent always gets a new physical key, even for equal fields. Visible scans never use this equality or the head as identity.
pub(crate) fn publish_tuple(
    session: &PersistentStorageSession,
    identity: CatalogObjectIdentity,
    tuple: &ConstraintTuple,
    replace_equal: bool,
) -> StorageBackendResult<PhysicalTuple> {
    let json = serde_json::value::to_raw_value(tuple)?;
    let class = CatalogOidClass::Constraint;
    let Some(predecessor) = super::current(session, class, identity)? else {
        return super::insert(session, class, identity, &json);
    };
    let mut equal = false;
    let key = predecessor.key();
    session
        .catalog
        .visit_metadata_with_key_length(&key, key.len(), &mut |key, stored| {
            equal = super::record(stored, key, class)?.payload.get() == json.get();
            Ok(())
        })?;
    if equal && !replace_equal {
        Ok(predecessor)
    } else {
        super::replace(session, class, predecessor, &json)
    }
}

fn error(error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error("publish physical catalog rows", &error)
}
