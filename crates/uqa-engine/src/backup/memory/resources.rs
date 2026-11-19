//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resource membership selects one immutable child and retains its existing codec.

use crate::state::SnapshotCut;
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    image_io::ImageReader, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

pub(super) use uqa_storage::backup::MEMORY_IMAGE_RESOURCE_KINDS as RESOURCE_KINDS;

pub(super) fn next(
    source: &SnapshotCut,
    kind: u8,
    after: Option<&str>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(Budgeted<String>, ImageReader<'static>)>> {
    macro_rules! resource {
        ($field:ident) => {{
            let retained = source.resources.$field.captured(&source.cut)?;
            let catalog = retained.get()?;
            let Some(name) = catalog.next_name(after)? else {
                return Ok(None);
            };
            let child = catalog.image(name)?.ok_or_else(|| {
                StorageBackendError::Other("captured resource identity has no image".into())
            })?;
            let name = ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            )
            .copy_text(name)?
            .into_budgeted()
            .expect("retained resource cursor name");
            let reader = child.into_image_reader(control)?;
            source.cut.check()?;
            Some((name, reader))
        }};
    }
    source.cut.check()?;
    Ok(match kind {
        1 => resource!(graphs),
        2 => resource!(models),
        3 => resource!(path_indexes),
        4 => resource!(named_analyzers),
        _ => None,
    })
}
