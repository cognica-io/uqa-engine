//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind selected column accelerators and evaluated writes to immutable logical index addresses.

use std::collections::BTreeMap;
use uqa_core::{CancellationToken, DocId, Predicate, Value};
use uqa_sql::{
    ast::{ColumnDef, TableKeyConstraint},
    SQLError,
};
use uqa_storage::{
    document_store::Document,
    mvcc::{SerializableKeySpace, SerializablePredicate, SerializableSession},
    read_control::StorageReadControl,
    PersistentStorageBackend, ValueIndexEntry, ValueIndexKey,
};

use super::{index_key::ScalarIndexDomain, SerializableRelationRead};
use crate::{
    catalog::index::physical::{rebuild::IndexDocuments, PhysicalIndexDefinitions},
    storage_errors::storage_error,
};

fn binding(columns: &[ColumnDef], name: &str) -> Result<([u8; 16], ScalarIndexDomain), SQLError> {
    let column = columns
        .iter()
        .find(|column| column.name == name)
        .ok_or_else(|| SQLError::UnknownColumn(name.into()))?;
    let identity = column
        .object_id
        .filter(|identity| *identity != [0; 16])
        .ok_or_else(|| SQLError::Internal("column index has no immutable identity".into()))?;
    let domain = ScalarIndexDomain::from_column_type(&column.ty).ok_or_else(|| {
        SQLError::Unsupported(format!(
            "serializable column index keys for {:?}",
            column.ty
        ))
    })?;
    Ok((identity, domain))
}

impl SerializableRelationRead {
    /// Register after the executing column access path selects its predicate and before exposing a result. Catalog estimates, hydration and constraint probes do not call this boundary.
    pub fn observe_column_index(
        &self,
        columns: &[ColumnDef],
        field: &ValueIndexKey,
        predicate: &Predicate,
    ) -> Result<(), SQLError> {
        let ValueIndexKey::Column(name) = field else {
            return Err(SQLError::Internal(
                "column index read requires a column accelerator".into(),
            ));
        };
        let (identity, domain) = binding(columns, name)?;
        domain.visit_predicate(predicate, &self.control, &mut |range| {
            let (lower, upper) = range.bounds();
            self.observe(SerializablePredicate::range(
                self.object,
                SerializableKeySpace::Index(identity),
                lower,
                upper,
            ))
        })
    }
}

pub struct SerializableColumnWrites<'a> {
    session: &'a dyn SerializableSession,
    backend: &'a dyn PersistentStorageBackend,
    object: [u8; 16],
    control: StorageReadControl,
}

pub struct ColumnIndexChange<'a> {
    pub table: &'a str,
    pub columns: &'a [ColumnDef],
    pub constraints: &'a [TableKeyConstraint],
    pub definitions: &'a PhysicalIndexDefinitions,
    pub documents: &'a dyn IndexDocuments,
    pub doc_id: DocId,
    pub had_old: bool,
    pub cached_old: Option<&'a BTreeMap<ValueIndexKey, Value>>,
    pub new: Option<&'a BTreeMap<ValueIndexKey, Value>>,
}

impl<'a> SerializableColumnWrites<'a> {
    pub fn new(
        backend: Option<&'a dyn PersistentStorageBackend>,
        object: Option<[u8; 16]>,
        cancellation: &CancellationToken,
    ) -> Result<Option<Self>, SQLError> {
        let (Some(backend), Some(object)) = (backend, object) else {
            return Ok(None);
        };
        let Some(session) = backend.serializable_session() else {
            return Ok(None);
        };
        let Some(context) = session
            .serializable_read_context()
            .map_err(|error| storage_error("retain serializable index writer", &error))?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            session,
            backend,
            object,
            control: context.read_control(cancellation),
        }))
    }

    /// Observe the original and replacement column keys before any private index or row publication. Savepoint ownership remains with the surrounding statement.
    pub fn observe(&self, change: ColumnIndexChange<'_>) -> Result<(), SQLError> {
        let fields = change
            .definitions
            .indexable_fields(change.table, change.columns, change.constraints)
            .map_err(|error| storage_error("bind serializable column indexes", &error))?;
        let mut projected_old: Option<Option<Document>> = None;
        for field in fields {
            let ValueIndexKey::Column(name) = &field else {
                continue;
            };
            let (identity, domain) = binding(change.columns, name)?;
            if change.had_old {
                if let Some(value) = change.cached_old.and_then(|values| values.get(&field)) {
                    self.observe_value(identity, domain, value)?;
                } else {
                    match self
                        .backend
                        .read_btree_index_entry(change.table, &field, change.doc_id)
                        .map_err(|error| storage_error("read original index key", &error))?
                    {
                        ValueIndexEntry::Present(value) => {
                            self.observe_value(identity, domain, &value)?;
                        }
                        ValueIndexEntry::Absent | ValueIndexEntry::Unbuilt => {
                            if projected_old.is_none() {
                                projected_old =
                                    Some(change.documents.read().get(change.doc_id).map_err(
                                        |error| {
                                            storage_error("project original column key", &error)
                                        },
                                    )?);
                            }
                            if let Some(document) = projected_old.as_ref().and_then(Option::as_ref)
                            {
                                self.observe_value(
                                    identity,
                                    domain,
                                    crate::catalog::index::physical::column_value(document, name),
                                )?;
                            }
                        }
                    }
                }
            }
            if let Some(values) = change.new {
                let value = values.get(&field).ok_or_else(|| {
                    SQLError::Internal(format!("replacement index key missing for {field}"))
                })?;
                self.observe_value(identity, domain, value)?;
            }
        }
        Ok(())
    }

    fn observe_value(
        &self,
        identity: [u8; 16],
        domain: ScalarIndexDomain,
        value: &Value,
    ) -> Result<(), SQLError> {
        let key = domain.encode(value, &self.control)?;
        self.session
            .observe_serializable_write(SerializablePredicate::point(
                self.object,
                SerializableKeySpace::Index(identity),
                &key,
            ))
            .map_err(|error| storage_error("observe serializable index write", &error))
    }
}
