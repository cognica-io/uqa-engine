//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained table metadata selects each field's original resolved analyzer binding.

use crate::state::SnapshotCut;
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    image_io::ImageReader, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

pub(super) type Key = (String, String);

pub(super) fn next(
    source: &SnapshotCut,
    after: Option<&Key>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(Budgeted<Key>, ImageReader<'static>)>> {
    source.cut.check()?;
    let retained = source.metadata.captured(&source.cut)?;
    let fields = &retained.get()?.analyzers;
    let Some(key) = fields.next_key(after)? else {
        return Ok(None);
    };
    let child = fields.binding(key)?.ok_or_else(|| {
        StorageBackendError::Other("captured field identity has no analyzer binding".into())
    })?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let (table, table_memory) = production.copy_text(&key.0)?.into_parts();
    let (field, field_memory) = production.copy_text(&key.1)?.into_parts();
    let key = production
        .finish(
            (table, field),
            production.combine(table_memory, field_memory),
        )?
        .into_budgeted()
        .expect("retained field binding cursor key");
    let reader = ImageReader::from_owner(child, control)?;
    source.cut.check()?;
    Ok(Some((key, reader)))
}
