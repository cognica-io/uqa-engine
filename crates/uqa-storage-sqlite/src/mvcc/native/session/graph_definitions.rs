//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stage semantic definition writes with the same original rows and evaluated batch as native publication.

use rusqlite::types::ValueRef;
use uqa_storage::catalog::graph_observations::{GraphDefinitionKey, GraphDefinitionKind};
use uqa_storage::KeyValueBatch;

use super::{Family, NativeRecordOwner, NativeSnapshot, Result};

fn kind(family: Family) -> Option<GraphDefinitionKind> {
    match family {
        Family::NamedGraphs | Family::StandaloneGraphCatalog => {
            Some(GraphDefinitionKind::NamedGraph)
        }
        Family::PathIndexes => Some(GraphDefinitionKind::PathIndex),
        _ => None,
    }
}

fn string(value: ValueRef<'_>) -> Result<&str> {
    value.as_str().map_err(|_| {
        crate::SQLiteError::StorageBackend("invalid evaluated graph definition name".into())
    })
}

impl NativeSnapshot {
    fn observe_graph_definition(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        kind: GraphDefinitionKind,
        row: &[ValueRef<'_>],
    ) -> Result<()> {
        let standalone = family == Family::StandaloneGraphCatalog;
        let scope = standalone.then(|| string(row[0])).transpose()?;
        let name = string(row[usize::from(standalone)])?;
        batch.observe_serializable_write(
            GraphDefinitionKey::new(self.graph_identifier_namespace(scope)?, kind, Some(name))
                .predicate(),
        )?;
        Ok(())
    }

    pub(super) fn observe_graph_definition_put(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        row: &[ValueRef<'_>],
    ) -> Result<()> {
        let Some(kind) = kind(family).filter(|_| batch.serializable_participant().is_some()) else {
            return Ok(());
        };
        let keys = 1 + usize::from(family == Family::StandaloneGraphCatalog);
        let unchanged = if family == Family::PathIndexes {
            self.read_row(family, owner, &row[..keys], |old| Ok(old[1] == row[1]))?
                .unwrap_or(false)
        } else {
            self.contains_row(family, owner, &row[..keys])?
        };
        if !unchanged {
            self.observe_graph_definition(batch, family, kind, row)?;
        }
        Ok(())
    }

    pub(super) fn observe_graph_definition_delete(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        prefix: &[ValueRef<'_>],
    ) -> Result<()> {
        let Some(kind) = kind(family).filter(|_| batch.serializable_participant().is_some()) else {
            return Ok(());
        };
        let keys = 1 + usize::from(family == Family::StandaloneGraphCatalog);
        if prefix.len() == keys {
            if self.contains_row(family, owner, prefix)? {
                self.observe_graph_definition(batch, family, kind, prefix)?;
            }
            return Ok(());
        }
        self.visit_paged_owned_rows(family, owner, prefix, |row| {
            self.observe_graph_definition(batch, family, kind, row)?;
            Ok(true)
        })
    }
}
