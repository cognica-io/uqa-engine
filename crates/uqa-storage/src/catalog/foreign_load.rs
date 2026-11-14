//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign definitions use the original complete physical and ACL boundaries before result ordering.

use super::{
    ordering::compare_names,
    relation_acl::replacements::Records,
    table_input::{CatalogTableInput, CatalogTableInputStep},
    ForeignTableRow,
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::cmp::Ordering;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryReservation},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Clone, Copy)]
pub enum CatalogForeignTableOrder {
    Physical,
    Relation,
}

#[derive(Debug)]
pub struct CatalogForeignTable {
    row: Box<ForeignTableRow>,
    memory: MemoryReservation,
    position: usize,
}

impl std::ops::Deref for CatalogForeignTable {
    type Target = ForeignTableRow;

    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

impl CatalogForeignTable {
    pub fn into_budgeted(self) -> Budgeted<Box<ForeignTableRow>> {
        Budgeted::new(self.row, self.memory)
    }
}

pub type CatalogForeignTables = BudgetedVec<CatalogForeignTable>;

pub struct CatalogForeignTableLoad {
    input: ImageDecoder<CatalogForeignTables, StorageBackendError>,
}

impl CatalogForeignTableLoad {
    /// Consume a complete physical definition and ACL source under its original controls. No row is exposed until all physical decoding, ordered replacements and result ordering succeed.
    pub fn new(
        source: CatalogTableInput<ForeignTableRow>,
        order: CatalogForeignTableOrder,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !source.control().shares_context(control) {
            return Err(StorageBackendError::Other(
                "foreign table input and loader must share their original control".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move { assemble(source, order, &retained, reader.input()).await },
        )?;
        Ok(Self { input })
    }

    /// Bound physical visits, ACL application and stable ordering by native work polls. All terminal paths retire the complete unpublished catalog and retained source.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogForeignTables>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn assemble(
    mut source: CatalogTableInput<ForeignTableRow>,
    order: CatalogForeignTableOrder,
    control: &StorageReadControl,
    mut progress: AsyncImageInput<'_>,
) -> StorageBackendResult<CatalogForeignTables> {
    let mut tables = CatalogForeignTables::new(control.memory());
    let mut replacements = Records::new();
    loop {
        progress.checkpoint().await?;
        match source.advance()? {
            CatalogTableInputStep::Pending | CatalogTableInputStep::DefinitionsComplete => (),
            CatalogTableInputStep::Definition(row) => {
                let (row, memory) = row.into_parts();
                let position = tables.len();
                tables.push(CatalogForeignTable {
                    row,
                    memory,
                    position,
                })?;
            }
            CatalogTableInputStep::AclReplacement(row) => replacements.insert(row, control)?,
            CatalogTableInputStep::Complete => break,
        }
    }
    drop(source);
    for table in tables.iter_mut() {
        let row = &mut table.row;
        replacements
            .apply(
                &row.relation,
                &mut row.security,
                &mut table.memory,
                &mut progress,
            )
            .await?;
    }
    drop(replacements);
    if matches!(order, CatalogForeignTableOrder::Relation) {
        sort_by_with_progress(&mut tables, &mut Progress(&mut progress)).await?;
    }
    control.check()?;
    Ok(tables)
}

struct Progress<'a, 'b>(&'a mut AsyncImageInput<'b>);

impl SortProgress<CatalogForeignTable, StorageBackendError> for Progress<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }

    async fn compare(
        &mut self,
        left: &CatalogForeignTable,
        right: &CatalogForeignTable,
    ) -> StorageBackendResult<Ordering> {
        let mut order =
            compare_names(&left.relation.schema, &right.relation.schema, self.0).await?;
        if order == Ordering::Equal {
            order = compare_names(&left.relation.name, &right.relation.name, self.0).await?;
        }
        Ok(order.then_with(|| left.position.cmp(&right.position)))
    }
}

#[cfg(test)]
mod tests;
