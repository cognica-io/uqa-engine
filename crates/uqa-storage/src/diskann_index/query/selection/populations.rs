//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse selections retain exact populations relative to their actual physical build.

use super::{invalid, DiskANNReadChanges, DiskANNReadSnapshot};
use crate::{
    diskann_index::{
        format::DiskANNCanonicalOrigin, pages::DiskANNOriginReader, DiskANNCanonicalCounts,
        DiskANNCanonicalRead, DiskANNQueryRead,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};

pub(super) fn capture(
    base: &DiskANNReadSnapshot,
    changes: &DiskANNReadChanges,
    built: &DiskANNOriginReader,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
    let input = built.manifest().input();
    base.check_control(control)?;
    changes.control.check()?;
    built.check_control(control)?;
    if input.dimensions != base.dimensions() {
        return Err(invalid("selected population dimensions differ from build"));
    }
    let mut counts = if changes.complete {
        DiskANNCanonicalCounts::default()
    } else {
        let Some(counts) = base.population_counts(input.generation, control)? else {
            return Ok(None);
        };
        counts
    };
    for (document, selected) in changes.sources.iter() {
        base.check_control(control)?;
        changes.control.check()?;
        let previous = if changes.complete {
            None
        } else {
            base.document_origin(*document, control)?
        };
        let replacement = selected
            .as_ref()
            .map(|source| source.document_origin(*document, control))
            .transpose()?
            .flatten();
        let covered = if previous.is_some() || replacement.is_some() {
            built.origin(*document, control)?
        } else {
            None
        };
        counts = counts.substituted(
            contribution(previous, covered, input.dimensions)?,
            contribution(replacement, covered, input.dimensions)?,
        )?;
    }
    base.check_control(control)?;
    changes.control.check()?;
    built.check_control(control)?;
    Ok(Some(counts))
}

fn contribution(
    origin: Option<DiskANNCanonicalOrigin>,
    built: Option<DiskANNCanonicalOrigin>,
    dimensions: u32,
) -> StorageBackendResult<DiskANNCanonicalCounts> {
    let Some(origin) = origin else {
        return Ok(DiskANNCanonicalCounts::default());
    };
    if origin.dimensions() != dimensions {
        return Err(invalid(
            "selected population origin dimensions differ from field",
        ));
    }
    let covered = built.is_some_and(|built| built.version() == origin.version());
    if covered && built != Some(origin) {
        return Err(invalid(
            "one canonical origin has inconsistent tensor shapes",
        ));
    }
    DiskANNCanonicalCounts::new(origin.count(), if covered { 0 } else { origin.count() })
}
