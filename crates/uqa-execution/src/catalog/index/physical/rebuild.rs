//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project physical rebuild batches while releasing retained document guards before SQL callbacks.

use super::PhysicalIndexDefinitions;
use crate::mutation::constraints::index_keys::IndexExpressionContext;
use std::ops::Deref;
use uqa_core::{DocId, Value};
use uqa_storage::{DocumentStore, StorageBackendError, StorageBackendResult, ValueIndexKey};

pub trait IndexDocuments {
    fn read(&self) -> Box<dyn Deref<Target = Box<dyn DocumentStore>> + '_>;
}

pub fn project(
    documents: &dyn IndexDocuments,
    definitions: &PhysicalIndexDefinitions,
    expressions: IndexExpressionContext<'_>,
    table: &str,
    fields: &[ValueIndexKey],
    ids: &[DocId],
) -> StorageBackendResult<Vec<Vec<(DocId, Value)>>> {
    let columns = fields
        .iter()
        .map(|field| match field {
            ValueIndexKey::Column(name) => Some(name.as_str()),
            ValueIndexKey::Index(_) => None,
        })
        .collect::<Option<Vec<_>>>();
    let mut result = fields
        .iter()
        .map(|_| Vec::with_capacity(ids.len()))
        .collect::<Vec<_>>();
    for chunk in ids.chunks(256) {
        if let Some(columns) = &columns {
            let store = documents.read();
            let mut projected = store.get_fields_multi(chunk, columns)?;
            for id in chunk {
                let Some(values) = projected.remove(id) else {
                    if store.get(*id)?.is_none() {
                        continue;
                    }
                    return Err(invalid(format!(
                        "value-index rebuild for {table} lost document {id}"
                    )));
                };
                if values.len() != fields.len() {
                    return Err(invalid(format!(
                        "value-index rebuild for {table} returned {} fields; expected {}",
                        values.len(),
                        fields.len()
                    )));
                }
                for (index, value) in values.into_iter().enumerate() {
                    result[index].push((*id, value));
                }
            }
        } else {
            let batch = {
                let store = documents.read();
                chunk
                    .iter()
                    .map(|id| store.get(*id).map(|document| (*id, document)))
                    .collect::<StorageBackendResult<Vec<_>>>()?
            };
            for (id, document) in batch {
                let Some(document) = document else {
                    continue;
                };
                let values = definitions
                    .document_values(expressions, table, fields, &document)
                    .map_err(|error| StorageBackendError::backend("index expression", error))?;
                for (index, field) in fields.iter().enumerate() {
                    result[index].push((
                        id,
                        values
                            .get(field)
                            .cloned()
                            .ok_or_else(|| invalid("missing prepared physical key"))?,
                    ));
                }
            }
        }
    }
    Ok(result)
}

fn invalid(message: impl ToString) -> StorageBackendError {
    StorageBackendError::Other(message.to_string())
}
