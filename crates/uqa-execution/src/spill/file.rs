//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequential immutable spill segments sharing one encrypted physical file.

use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_storage::temporary_file::TemporaryFile;

use super::{append_batches, spill_error, SPILL_MAGIC};
use crate::{Batch, ExecResult};

#[derive(Clone)]
pub(crate) struct SpillFileArena {
    file: Arc<Mutex<Option<TemporaryFile>>>,
    directory: Option<PathBuf>,
}

impl SpillFileArena {
    pub(crate) fn new(directory: Option<PathBuf>) -> Self {
        Self {
            file: Arc::new(Mutex::new(None)),
            directory,
        }
    }

    /// Only the newest segment may grow. Older readers keep their sealed byte range and cannot read a later segment.
    pub(super) fn append(
        &self,
        batches: &[Batch],
        range: &mut Option<Range<u64>>,
    ) -> ExecResult<TemporaryFile> {
        let mut owner = self.file.lock();
        if owner.is_none() {
            *owner = Some(create(self.directory.as_deref())?);
        }
        let file = owner.as_mut().expect("spill arena has a file");
        let start = file
            .metadata()
            .map_err(|error| spill_error(error.to_string()))?
            .len();
        if range.as_ref().is_some_and(|range| range.end != start) {
            return Err(spill_error("cannot append to a sealed spill segment"));
        }
        // Obtain the retained handle before publication, so even a handle error cannot leave duplicated appended rows on retry.
        let retained = file
            .reopen()
            .map_err(|error| spill_error(error.to_string()))?;
        append_batches(file, batches)?;
        let end = file
            .metadata()
            .map_err(|error| spill_error(error.to_string()))?
            .len();
        *range = Some(range.as_ref().map_or(start, |range| range.start)..end);
        Ok(retained)
    }
}

pub(super) fn create(directory: Option<&Path>) -> ExecResult<TemporaryFile> {
    let mut file = match directory {
        Some(directory) => TemporaryFile::new_in(directory).map_err(|error| {
            spill_error(format!(
                "failed to create spill file in {}: {error}",
                directory.display()
            ))
        })?,
        None => TemporaryFile::new()
            .map_err(|error| spill_error(format!("failed to create spill file: {error}")))?,
    };
    file.write_all(SPILL_MAGIC)
        .map_err(|error| spill_error(format!("failed to initialize spill file: {error}")))?;
    file.flush()
        .map_err(|error| spill_error(format!("failed to flush spill header: {error}")))?;
    Ok(file)
}

#[cfg(test)]
mod tests;
