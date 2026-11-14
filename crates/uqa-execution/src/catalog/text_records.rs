//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical metadata families precede their owner's semantic interpretation.

use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec},
};
use uqa_storage::{
    catalog::{
        input::CatalogInputLimits,
        text_input::{CatalogTextRow, CatalogTextStep},
    },
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendResult,
};

pub(crate) async fn rows(
    catalog: &dyn CatalogFacade,
    prefix: &str,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<BudgetedVec<Budgeted<CatalogTextRow>>> {
    work.checkpoint().await?;
    let mut source = catalog.begin_metadata_input(prefix, limits, control)?;
    let mut rows = BudgetedVec::new(control.memory());
    loop {
        work.checkpoint().await?;
        match source.advance()? {
            CatalogTextStep::Pending => (),
            CatalogTextStep::Row(row) => rows.push(row)?,
            CatalogTextStep::Complete => return Ok(rows),
        }
    }
}

pub(crate) fn claim_oid(text: &str) -> Option<u32> {
    if text.is_empty()
        || text.len() > 10
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    text.parse().ok()
}
