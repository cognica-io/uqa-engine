//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog and standalone registry mutations supply their original rows to common structural observation.

use rusqlite::types::ValueRef;
use uqa_storage::{catalog::graph_observations::observe_label_registry_change, KeyValueBatch};

use super::{Family, NativeRecordOwner, NativeSnapshot, Result};

fn string(value: ValueRef<'_>) -> Result<&str> {
    value
        .as_str()
        .map_err(|_| crate::SQLiteError::StorageBackend("invalid graph registry row".into()))
}

fn address<'a>(
    family: Family,
    row: &[ValueRef<'a>],
) -> Result<Option<(Option<&'a str>, &'a str, usize)>> {
    match family {
        Family::StandaloneGraphCatalog => Ok(Some((Some(string(row[0])?), string(row[1])?, 2))),
        Family::Metadata => Ok(string(row[0])?
            .strip_prefix("graph_label_registry::")
            .map(|graph| (None, graph, 1))),
        _ => Ok(None),
    }
}

impl NativeSnapshot {
    pub(super) fn observe_graph_labels_put(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        row: &[ValueRef<'_>],
    ) -> Result<()> {
        if batch.serializable_participant().is_none() {
            return Ok(());
        }
        let Some((scope, graph, value)) = address(family, row)? else {
            return Ok(());
        };
        let namespace = self.graph_identifier_namespace(scope)?;
        let new = Some(string(row[value])?);
        let found = self.read_row(family, owner, &row[..value], |old| {
            Ok(observe_label_registry_change(
                namespace,
                graph,
                Some(string(old[value])?),
                new,
                batch,
                &self.control,
            )?)
        })?;
        if found.is_none() {
            observe_label_registry_change(namespace, graph, None, new, batch, &self.control)?;
        }
        Ok(())
    }

    pub(super) fn observe_graph_labels_delete(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        prefix: &[ValueRef<'_>],
    ) -> Result<()> {
        if batch.serializable_participant().is_none()
            || !matches!(family, Family::Metadata | Family::StandaloneGraphCatalog)
        {
            return Ok(());
        }
        let mut observe = |row: &[ValueRef<'_>]| {
            if let Some((scope, graph, value)) = address(family, row)? {
                observe_label_registry_change(
                    self.graph_identifier_namespace(scope)?,
                    graph,
                    Some(string(row[value])?),
                    None,
                    batch,
                    &self.control,
                )?;
            }
            Ok(())
        };
        let keys = if family == Family::Metadata { 1 } else { 2 };
        if prefix.len() == keys {
            if address(family, prefix)?.is_some() {
                self.read_row(family, owner, prefix, &mut observe)?;
            }
            return Ok(());
        }
        self.visit_paged_owned_rows(family, owner, prefix, |row| {
            observe(row)?;
            Ok(true)
        })
    }
}
