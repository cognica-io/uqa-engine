//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Read;

use sha2::{Digest, Sha256};

use super::{objects::SpanReader, BackupLease, LocalRepository};
use crate::{
    backup::{
        functions::FunctionDecoder, BackupError, BackupFunctionReference, BackupResult, BackupRoot,
        BackupSourceStep, FrameKind, ImageDigest, InventoryEntry, InventoryReader,
    },
    read_control::StorageReadControl,
};

impl LocalRepository {
    pub(super) fn function_entry(
        &self,
        root: BackupRoot,
        control: &StorageReadControl,
    ) -> BackupResult<Option<InventoryEntry>> {
        let span = self.span_reader(root.inventory(), FrameKind::Inventory, false, control)?;
        let mut inventory = InventoryReader::new(span, self.limits, control)?;
        if inventory.format() != root.cut().format {
            return Err(BackupError::Integrity);
        }
        let Some(entry) = inventory.read_next()? else {
            return Ok(None);
        };
        Ok((entry.kind() == crate::backup::UnitKind::FunctionReferences).then_some(entry))
    }
}

struct Input<'a> {
    span: SpanReader<'a>,
    decoder: FunctionDecoder,
    hash: Sha256,
}

/// Authenticated ordered declarations from the selected completed root. A yielded name remains provisional until the whole section and the enclosing image pass verification.
pub struct BackupFunctionReader<'a> {
    input: Option<Input<'a>>,
    expected: ImageDigest,
    complete: bool,
    selected: bool,
    control: StorageReadControl,
}

impl<'a> BackupLease<'a> {
    /// Read the optional first inventory unit without consuming or reopening this lease. A copied root uses the destination repository's own protection. Legacy roots return None.
    pub fn function_references(
        &self,
        control: &StorageReadControl,
    ) -> BackupResult<Option<BackupFunctionReader<'a>>> {
        let Some(entry) = self.repository.function_entry(self.root, control)? else {
            return Ok(None);
        };
        Ok(Some(BackupFunctionReader {
            input: Some(Input {
                span: self.repository.span_reader(
                    entry.object(),
                    FrameKind::Unit,
                    false,
                    control,
                )?,
                decoder: FunctionDecoder::new(Some(entry.decoded_bytes()), control),
                hash: Sha256::new(),
            }),
            expected: entry.digest(),
            complete: false,
            selected: false,
            control: control.clone(),
        }))
    }
}

impl BackupFunctionReader<'_> {
    pub const fn expected_digest(&self) -> ImageDigest {
        self.expected
    }

    pub fn current(&self) -> Option<BackupFunctionReference<'_>> {
        if self.selected {
            self.input.as_ref()?.decoder.current()
        } else {
            None
        }
    }

    /// Read at most one field and the supplied number of bytes. Errors retire the open file and any partially decoded name.
    pub fn advance(&mut self, maximum_bytes: usize) -> BackupResult<BackupSourceStep> {
        if self.complete {
            return Ok(BackupSourceStep::Complete);
        }
        let mut input = self.input.take().ok_or(BackupError::Finished)?;
        self.selected = false;
        self.control.check()?;
        if maximum_bytes == 0 {
            return Err(BackupError::Invalid(
                "zero registered SQL function read window",
            ));
        }
        let mut buffer = [0; 4096];
        let size = buffer
            .len()
            .min(maximum_bytes)
            .min(input.decoder.remaining());
        let bytes = input.span.read(&mut buffer[..size])?;
        if bytes == 0 {
            input.decoder.finish()?;
            if ImageDigest::from_bytes(input.hash.finalize().into()) != self.expected {
                return Err(BackupError::Integrity);
            }
            self.complete = true;
            return Ok(BackupSourceStep::Complete);
        }
        input.hash.update(&buffer[..bytes]);
        self.selected = input.decoder.accept(&buffer[..bytes])?;
        self.input = Some(input);
        Ok(if self.selected {
            BackupSourceStep::Unit
        } else {
            BackupSourceStep::Pending
        })
    }
}
