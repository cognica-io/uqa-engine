//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Comment eligibility follows the addressed objects of the original transaction generation.

use super::{CapturedComments, CommentAddress, CommentRegistry};
use crate::catalog::CatalogDefinitionSnapshot;
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{
    memory::{OwnedMap, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_sql::{
    ast::{
        ColumnDef, ForeignKey, RelationPersistence, TableCheck, TableHierarchy, TableKeyConstraint,
    },
    catalog::{
        dependencies::{
            ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, NAMESPACE_CLASS, PROCEDURE_CLASS,
            RELATION_CLASS, REWRITE_CLASS, TRIGGER_CLASS, TYPE_CLASS,
        },
        relation_oids::{RelationCatalogOids, RelationOidKind},
        temporary_namespace::{is_temporary_schema_name, TemporaryNamespaceOids},
    },
    SQLError,
};
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod events;
mod relations;
mod types;

#[cfg(test)]
mod tests;

/// Borrowed object metadata from the same generation as the comments and definition roots.
pub struct CommentTableSource<'a> {
    pub relation: &'a RelationIdentity,
    pub object_id: [u8; 16],
    pub catalog_oids: Option<RelationCatalogOids>,
    pub persistence: RelationPersistence,
    pub columns: &'a [ColumnDef],
    pub checks: &'a [TableCheck],
    pub keys: &'a [TableKeyConstraint],
    pub foreign_keys: &'a [ForeignKey],
    pub hierarchy: &'a TableHierarchy,
}

impl CommentTableSource<'_> {
    fn oids(&self) -> RelationCatalogOids {
        self.catalog_oids
            .unwrap_or_else(|| RelationCatalogOids::legacy(RelationOidKind::Table, &self.object_id))
    }
}

pub struct CommentCaptureSource {
    comments: Arc<CommentRegistry>,
}

struct Selection {
    comments: Arc<CommentRegistry>,
    excluded: BTreeSet<CommentAddress>,
}

impl CommentCaptureSource {
    /// Select addresses while the transaction owner retains one original catalog generation. The returned source retains neither the object metadata nor live Engine state.
    pub fn new<'a>(
        definitions: &CatalogDefinitionSnapshot,
        tables: impl IntoIterator<Item = CommentTableSource<'a>>,
        temporary_namespace: Option<TemporaryNamespaceOids>,
    ) -> Result<Self, SQLError> {
        let mut source = Selection {
            comments: definitions.comments.clone(),
            excluded: BTreeSet::new(),
        };
        if source.comments.is_empty() {
            return Ok(Self {
                comments: source.comments,
            });
        }
        let mut original_tables = OwnedMap::new();
        for table in tables {
            original_tables.insert(table.relation, table);
        }
        let scope = Scope {
            definitions,
            tables: original_tables,
            temporary_namespace,
        };
        scope.exclude_types(&mut source);
        scope.exclude_relations(&mut source)?;
        scope.exclude_events(&mut source)?;
        scope.exclude_routines(&mut source)?;
        scope.exclude_namespaces(&mut source);
        let mut comments = (*source.comments).clone();
        for address in source.excluded {
            comments.remove(&address)?;
        }
        Ok(Self {
            comments: Arc::new(comments),
        })
    }
}

impl Selection {
    fn has_class(&self, class_id: u32) -> bool {
        self.comments
            .keys()
            .any(|address| address.class_id == class_id)
    }

    fn exclude(&mut self, class_id: u32, object_id: u32) {
        let start = CommentAddress {
            object_id,
            sub_id: i32::MIN,
            class_id: 0,
        };
        let end = CommentAddress {
            object_id,
            sub_id: i32::MAX,
            class_id: u32::MAX,
        };
        self.excluded.extend(
            self.comments
                .range(start..=end)
                .filter(|(address, _)| address.class_id == class_id)
                .map(|(address, _)| *address),
        );
    }
}

impl MemoryCaptureSource for CommentCaptureSource {
    type Frozen = CapturedMemoryValue<CapturedComments>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = self
            .comments
            .capture(&production)
            .map_err(|error| StorageBackendError::backend("comment catalog capture", error))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("comment capture owns its allowance"));
        CapturedMemoryValue::new(value, control)
    }
}

struct Scope<'a, 'd> {
    definitions: &'d CatalogDefinitionSnapshot,
    tables: OwnedMap<&'a RelationIdentity, CommentTableSource<'a>>,
    temporary_namespace: Option<TemporaryNamespaceOids>,
}

impl Scope<'_, '_> {
    fn exclude_namespaces(&self, source: &mut Selection) {
        if let Some(namespace) = self.temporary_namespace {
            source.exclude(NAMESPACE_CLASS, namespace.namespace);
            source.exclude(NAMESPACE_CLASS, namespace.toast_namespace);
        }
        for (_, schema) in self
            .definitions
            .schemas
            .iter()
            .filter(|(name, _)| is_temporary_schema_name(name))
        {
            if let Some(tuple) = schema.tuple {
                source.exclude(NAMESPACE_CLASS, tuple.oid as u32);
            }
        }
    }

    fn table(&self, relation: &RelationIdentity) -> Option<&CommentTableSource<'_>> {
        self.tables.get_by(|key| relation.cmp(*key))
    }

    fn table_by_name(&self, name: &str) -> Result<Option<&CommentTableSource<'_>>, SQLError> {
        let name = RelationIdentity::borrowed_reference(name).ok_or_else(|| {
            SQLError::Internal("comment object has an invalid original relation name".into())
        })?;
        let schema = name.schema.unwrap_or(RelationName::decoded("public"));
        Ok(self.tables.get_by(|key| {
            schema
                .cmp(&RelationName::decoded(&key.schema))
                .then_with(|| name.name.cmp(&RelationName::decoded(&key.name)))
        }))
    }

    fn temporary_relation_oid(&self, relation: &RelationIdentity) -> Option<u32> {
        self.table(relation)
            .filter(|table| table.persistence == RelationPersistence::Temporary)
            .map(|table| table.oids().relation)
            .or_else(|| {
                self.definitions
                    .views
                    .get(relation)
                    .filter(|view| view.persistence == RelationPersistence::Temporary)
                    .map(|view| view.relation_oids().relation)
            })
            .or_else(|| {
                self.definitions
                    .foreign_tables
                    .get(relation)
                    .filter(|table| table.persistence == RelationPersistence::Temporary)
                    .map(|table| table.relation_oids().relation)
            })
    }
}
