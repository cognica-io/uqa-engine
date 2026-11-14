//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    corrupt,
    restoration::{self, Columns, Parameter, Restoration},
    validate_declared_field, GinAnalyzerOwners, GinAnalyzerTable, GinAnalyzerTables,
};
use std::{
    collections::BTreeMap,
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_sql::ast::IndexKey;
use uqa_storage::{CatalogIndexRow, RelationIdentity, StorageBackendResult};

struct Ordinary<'a, T> {
    tables: &'a T,
    owners: GinAnalyzerOwners,
}

struct OrdinaryColumns(std::vec::IntoIter<IndexKey>);

impl Columns for OrdinaryColumns {
    type Column = String;
    fn next(&mut self) -> Option<String> {
        self.0.find_map(|key| match key {
            IndexKey::Column(name) => Some(name),
            IndexKey::Expression(_) => None,
        })
    }
}

impl Parameter for BTreeMap<String, String> {
    fn name(&self) -> Option<&str> {
        self.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("analyzer"))
            .map(|(_, name)| name.trim())
    }
}

impl<T: GinAnalyzerTables> Restoration for Ordinary<'_, T> {
    type Name = String;
    type Table = T::Table;
    type Columns = OrdinaryColumns;
    type Parameter = BTreeMap<String, String>;
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(())
    }
    async fn name(&mut self, text: &str) -> StorageBackendResult<String> {
        RelationIdentity::from_legacy_name(text)
            .map(|relation| relation.qualified_name())
            .map_err(corrupt)
    }
    async fn table(&mut self, name: &str) -> StorageBackendResult<Option<T::Table>> {
        self.tables.table(name)
    }
    async fn columns(&mut self, text: &str) -> StorageBackendResult<Self::Columns> {
        Ok(OrdinaryColumns(
            serde_json::from_str::<Vec<IndexKey>>(text)?.into_iter(),
        ))
    }
    async fn parameter(&mut self, text: &str) -> StorageBackendResult<Self::Parameter> {
        Ok(serde_json::from_str(text)?)
    }
    async fn equal(&mut self, left: &str, right: &str) -> StorageBackendResult<bool> {
        Ok(left == right)
    }
    async fn register(&mut self, table: &T::Table, field: &str) -> StorageBackendResult<()> {
        table.register_field(field)
    }
    async fn validate(
        &mut self,
        table: &T::Table,
        name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        validate_declared_field(table, name, field)
    }
    async fn owner(
        &mut self,
        table: &str,
        field: String,
        analyzer: &str,
    ) -> StorageBackendResult<()> {
        if self
            .owners
            .insert((table.to_owned(), field), analyzer.to_owned())
            .is_some_and(|previous| previous != analyzer)
        {
            return Err(corrupt("GIN definitions have competing analyzer owners"));
        }
        Ok(())
    }
}

pub(super) fn restore_rows(
    rows: Vec<CatalogIndexRow>,
    tables: &impl GinAnalyzerTables,
) -> StorageBackendResult<GinAnalyzerOwners> {
    let mut work = Ordinary {
        tables,
        owners: GinAnalyzerOwners::new(),
    };
    for row in rows {
        let mut future = std::pin::pin!(restoration::restore_row(&row, &mut work));
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result?,
            Poll::Pending => unreachable!("ordinary GIN restoration never suspends"),
        }
    }
    Ok(work.owners)
}
