//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone graph records share the connection's logical snapshot and evaluated write batch.

mod keys;

use std::sync::Arc;

use rusqlite::types::ValueRef;
use uqa_core::{Edge, Vertex};
use uqa_graph::{
    begin_graph_write, GraphLabelRegistry, GraphStorage, GraphStoreError, GraphStoreResult,
    GraphWriteTransaction,
};
use uqa_storage::{GraphEntityFilter, GraphEntityKind, KeyValueBatch, PersistentStorageBackend};

use super::{
    decode_graph_id, decode_properties, encode_graph_id, sqlite_graph_error,
    TAGGED_PROPERTIES_FORMAT,
};
use crate::{
    mvcc::native::{NativeRecordFamily as Family, NativeRecordOwner, NativeSnapshot},
    ManagedConnection, Result, SQLiteError,
};

pub(super) struct NativeGraphStorage {
    pub connection: ManagedConnection,
    pub backend: Arc<dyn PersistentStorageBackend>,
    pub scope: String,
}

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}
fn string(value: ValueRef<'_>) -> Result<&str> {
    value
        .as_str()
        .map_err(|_| SQLiteError::StorageBackend("graph text field has invalid encoding".into()))
}
fn integer(value: ValueRef<'_>) -> Result<i64> {
    value
        .as_i64()
        .map_err(|_| SQLiteError::StorageBackend("graph integer field has invalid encoding".into()))
}
fn owner(snapshot: &NativeSnapshot) -> NativeRecordOwner {
    NativeRecordOwner::Database(snapshot.database)
}
fn family(kind: GraphEntityKind) -> Family {
    match kind {
        GraphEntityKind::Vertex => Family::StandaloneGraphVertices,
        GraphEntityKind::Edge => Family::StandaloneGraphEdges,
    }
}

impl NativeGraphStorage {
    fn snapshot(&self) -> Result<Arc<NativeSnapshot>> {
        self.connection
            .native_snapshot()?
            .ok_or(SQLiteError::SessionMappingMismatch)
    }
    fn read<T>(&self, read: impl FnOnce(&NativeSnapshot) -> Result<T>) -> GraphStoreResult<T> {
        self.snapshot()
            .and_then(|snapshot| read(&snapshot))
            .map_err(|error| sqlite_graph_error(&error))
    }
    fn write<T>(
        &self,
        write: impl FnOnce(&NativeSnapshot, &mut dyn KeyValueBatch) -> Result<T>,
    ) -> GraphStoreResult<T> {
        self.connection
            .with_native_write(write)
            .and_then(|result| result.ok_or(SQLiteError::SessionMappingMismatch))
            .map_err(|error| sqlite_graph_error(&error))
    }
    pub(super) fn ensure_tables(&self) -> Result<()> {
        let snapshot = self.snapshot()?;
        if snapshot.contains_row(
            Family::StandaloneGraphScopes,
            owner(&snapshot),
            &[text(&self.scope)],
        )? {
            return Ok(());
        }
        drop(snapshot);
        self.connection
            .with_native_write(|snapshot, batch| {
                if !snapshot.contains_row(
                    Family::StandaloneGraphScopes,
                    owner(snapshot),
                    &[text(&self.scope)],
                )? {
                    snapshot.put_row(
                        batch,
                        Family::StandaloneGraphScopes,
                        owner(snapshot),
                        &[text(&self.scope), ValueRef::Null],
                    )?;
                }
                Ok(())
            })?
            .ok_or(SQLiteError::SessionMappingMismatch)
    }
    pub(super) fn metadata(&self, key: &str) -> Result<Option<String>> {
        let snapshot = self.snapshot()?;
        snapshot.read_row(
            Family::StandaloneGraphMetadata,
            owner(&snapshot),
            &[text(&self.scope), text(key)],
            |row| Ok(string(row[2])?.to_owned()),
        )
    }
    pub(super) fn save_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.connection
            .with_native_write(|snapshot, batch| {
                if key == "identifier_generation" {
                    snapshot.fence_graph_identifier_scope(batch, Some(&self.scope))?;
                }
                snapshot.put_row(
                    batch,
                    Family::StandaloneGraphMetadata,
                    owner(snapshot),
                    &[text(&self.scope), text(key), text(value)],
                )
            })?
            .ok_or(SQLiteError::SessionMappingMismatch)
    }
    fn delete_entity(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        let id = encode_graph_id("entity", id).map_err(|error| sqlite_graph_error(&error))?;
        self.write(|snapshot, batch| {
            snapshot.visit_paged_rows(
                Family::StandaloneGraphMembership,
                &[
                    text(&self.scope),
                    text(kind.as_str()),
                    ValueRef::Integer(id),
                ],
                |row| {
                    snapshot.replace_graph_row(
                        batch,
                        Family::StandaloneGraphMembership,
                        row,
                        None,
                        false,
                    )?;
                    Ok(true)
                },
            )?;
            snapshot.replace_graph_row(
                batch,
                family(kind),
                &[text(&self.scope), ValueRef::Integer(id)],
                None,
                false,
            )
        })
    }
    fn membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
        present: bool,
    ) -> GraphStoreResult<()> {
        let id = encode_graph_id("entity", id).map_err(|error| sqlite_graph_error(&error))?;
        self.write(|snapshot, batch| {
            let row = [
                text(&self.scope),
                text(kind.as_str()),
                ValueRef::Integer(id),
                text(graph),
            ];
            if snapshot.contains_row(Family::StandaloneGraphMembership, owner(snapshot), &row)?
                == present
            {
                return Ok(());
            }
            snapshot.replace_graph_row(
                batch,
                Family::StandaloneGraphMembership,
                &row,
                present.then_some(&row),
                false,
            )
        })
    }
}

impl GraphStorage for NativeGraphStorage {
    fn guard_definition(&self, graph: Option<&str>) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            snapshot.guard_graph_definition(batch, Some(&self.scope), graph)
        })
    }
    fn identifiers(&self) -> GraphStoreResult<Option<uqa_graph::GraphIdentifierScope<'_>>> {
        let Some(allocator) = self.backend.identifier_allocator() else {
            return Ok(None);
        };
        let generation = uqa_graph::decode_identifier_generation(
            self.metadata("identifier_generation")
                .map_err(|error| sqlite_graph_error(&error))?
                .as_deref(),
        )?;
        Ok(Some(uqa_graph::GraphIdentifierScope::standalone(
            allocator,
            &self.scope,
            generation,
        )))
    }
    fn reset_identifiers(&self) -> GraphStoreResult<()> {
        let generation = uqa_storage::catalog::new_nonzero_catalog_identity("graph", "generation")?;
        let value = serde_json::to_string(&generation)
            .map_err(|error| GraphStoreError::CorruptGraph(error.to_string()))?;
        self.save_metadata("identifier_generation", &value)
            .map_err(|error| sqlite_graph_error(&error))
    }
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
        begin_graph_write(Arc::clone(&self.backend))
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        self.read(|snapshot| {
            keys::strings(
                snapshot,
                Family::StandaloneGraphCatalog,
                &[text(&self.scope)],
                1,
            )
        })
    }
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool> {
        self.read(|snapshot| {
            snapshot.contains_row(
                Family::StandaloneGraphCatalog,
                owner(snapshot),
                &[text(&self.scope), text(graph)],
            )
        })
    }
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            if !snapshot.contains_row(
                Family::StandaloneGraphCatalog,
                owner(snapshot),
                &[text(&self.scope), text(graph)],
            )? {
                snapshot.put_row(
                    batch,
                    Family::StandaloneGraphCatalog,
                    owner(snapshot),
                    &[text(&self.scope), text(graph), text("{}")],
                )?;
            }
            Ok(())
        })
    }
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            snapshot.fence_graph_definition(batch, Some(&self.scope), graph)?;
            snapshot.delete_prefix(
                batch,
                Family::StandaloneGraphCatalog,
                owner(snapshot),
                &[text(&self.scope), text(graph)],
            )
        })
    }
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        self.read(|snapshot| {
            snapshot
                .read_row(
                    Family::StandaloneGraphCatalog,
                    owner(snapshot),
                    &[text(&self.scope), text(graph)],
                    |row| Ok(serde_json::from_str(string(row[2])?)?),
                )?
                .ok_or_else(|| SQLiteError::StorageBackend(format!("missing graph {graph:?}")))
        })
    }
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            snapshot.fence_graph_definition(batch, Some(&self.scope), graph)?;
            if snapshot.contains_row(
                Family::StandaloneGraphCatalog,
                owner(snapshot),
                &[text(&self.scope), text(graph)],
            )? {
                let json = serde_json::to_string(registry)?;
                snapshot.put_row(
                    batch,
                    Family::StandaloneGraphCatalog,
                    owner(snapshot),
                    &[text(&self.scope), text(graph), text(&json)],
                )?;
            }
            Ok(())
        })
    }
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.metadata(&format!("next_{}_id", kind.as_str()))
            .map_err(|error| sqlite_graph_error(&error))?
            .map(|value| {
                value.parse().map_err(|error| {
                    GraphStoreError::CorruptGraph(format!("invalid graph id counter: {error}"))
                })
            })
            .transpose()
    }
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()> {
        self.save_metadata(&format!("next_{}_id", kind.as_str()), &next.to_string())
            .map_err(|error| sqlite_graph_error(&error))
    }
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.read(|snapshot| {
            snapshot.read_row(
                Family::StandaloneGraphVertices,
                owner(snapshot),
                &[
                    text(&self.scope),
                    ValueRef::Integer(encode_graph_id("vertex", id)?),
                ],
                |row| {
                    Ok(Vertex {
                        vertex_id: id,
                        label: string(row[2])?.to_owned(),
                        properties: decode_properties(string(row[3])?, integer(row[4])?)?,
                    })
                },
            )
        })
    }
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        self.read(|snapshot| {
            snapshot.read_row(
                Family::StandaloneGraphEdges,
                owner(snapshot),
                &[
                    text(&self.scope),
                    ValueRef::Integer(encode_graph_id("edge", id)?),
                ],
                |row| {
                    Ok(Edge {
                        edge_id: id,
                        source_id: decode_graph_id("source", integer(row[2])?)?,
                        target_id: decode_graph_id("target", integer(row[3])?)?,
                        label: string(row[4])?.to_owned(),
                        properties: decode_properties(string(row[5])?, integer(row[6])?)?,
                    })
                },
            )
        })
    }
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            let id = ValueRef::Integer(encode_graph_id("vertex", vertex.vertex_id)?);
            let json = serde_json::to_string(&vertex.properties)?;
            snapshot.replace_graph_row(
                batch,
                Family::StandaloneGraphVertices,
                &[text(&self.scope), id],
                Some(&[
                    text(&self.scope),
                    id,
                    text(&vertex.label),
                    text(&json),
                    ValueRef::Integer(TAGGED_PROPERTIES_FORMAT),
                ]),
                false,
            )
        })
    }
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()> {
        self.write(|snapshot, batch| {
            let id = ValueRef::Integer(encode_graph_id("edge", edge.edge_id)?);
            let json = serde_json::to_string(&edge.properties)?;
            snapshot.replace_graph_row(
                batch,
                Family::StandaloneGraphEdges,
                &[text(&self.scope), id],
                Some(&[
                    text(&self.scope),
                    id,
                    ValueRef::Integer(encode_graph_id("source", edge.source_id)?),
                    ValueRef::Integer(encode_graph_id("target", edge.target_id)?),
                    text(&edge.label),
                    text(&json),
                    ValueRef::Integer(TAGGED_PROPERTIES_FORMAT),
                ]),
                false,
            )
        })
    }
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()> {
        self.delete_entity(GraphEntityKind::Vertex, id)
    }
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()> {
        self.delete_entity(GraphEntityKind::Edge, id)
    }
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        uqa_storage::catalog::validate_graph_page(limit)?;
        self.read(|snapshot| {
            let mut ids = Vec::new();
            snapshot.visit_graph_ids(Some(&self.scope), filter, after, |id| {
                ids.push(decode_graph_id("graph entity", id)?);
                Ok(ids.len() < limit)
            })?;
            Ok(ids)
        })
    }
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64> {
        self.read(|snapshot| {
            let mut count = 0_u64;
            snapshot.visit_graph_ids(Some(&self.scope), filter, None, |_| {
                count = count.checked_add(1).ok_or_else(|| {
                    SQLiteError::StorageBackend("graph entity count overflow".into())
                })?;
                Ok(true)
            })?;
            Ok(count)
        })
    }
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.read(|snapshot| {
            let mut maximum = None;
            snapshot.visit_graph_ids(
                Some(&self.scope),
                GraphEntityFilter::new(kind, None),
                None,
                |id| {
                    maximum = Some(decode_graph_id("entity", id)?);
                    Ok(true)
                },
            )?;
            Ok(maximum)
        })
    }
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>> {
        self.read(|snapshot| {
            keys::strings(
                snapshot,
                Family::StandaloneGraphMembership,
                &[
                    text(&self.scope),
                    text(kind.as_str()),
                    ValueRef::Integer(encode_graph_id("entity", id)?),
                ],
                3,
            )
        })
    }
    fn has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> GraphStoreResult<bool> {
        self.read(|snapshot| {
            snapshot.contains_row(
                Family::StandaloneGraphMembership,
                owner(snapshot),
                &[
                    text(&self.scope),
                    text(kind.as_str()),
                    ValueRef::Integer(encode_graph_id("entity", id)?),
                    text(graph),
                ],
            )
        })
    }
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.membership(kind, id, graph, true)
    }
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.membership(kind, id, graph, false)
    }
}
