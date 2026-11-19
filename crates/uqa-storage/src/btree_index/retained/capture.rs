//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native immutable roots retain evaluated keys without decoding or copying their population.

use super::{
    records::{decode_id, invalid, key, Node, NODE},
    RetainedValueIndex,
};
use crate::{
    image_io::{ImageInput, ImageReader},
    mvcc::{PrivateRecordSnapshot, VersionError},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::io::Read;
use uqa_core::{memory::Budgeted, DocId, Value};

mod image;

/// Independently admitted immutable native records. Resident chunks and run metadata retain their complete cached footprint; spilled payloads remain in their original encrypted files. This owner contains no writable index or decoded value population.
pub struct RetainedValueIndexSnapshot {
    view: PrivateRecordSnapshot,
    len: usize,
    control: StorageReadControl,
}

impl RetainedValueIndex {
    /// Retain the existing native roots without spilling, compaction, value reads or traversal of the ordered index. Admission precedes publication and preserves the invoking cancellation signal.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<RetainedValueIndexSnapshot>> {
        control.check()?;
        let memory = control
            .memory()
            .reserve(size_of::<RetainedValueIndexSnapshot>())?;
        let view = self
            .records
            .capture_native_records(control)
            .map_err(failure)?;
        control.check()?;
        Ok(Budgeted::new(
            RetainedValueIndexSnapshot {
                view,
                len: self.len,
                control: control.clone(),
            },
            memory,
        ))
    }
}

impl RetainedValueIndexSnapshot {
    pub fn len(&self) -> StorageBackendResult<usize> {
        self.control.check()?;
        Ok(self.len)
    }

    pub fn is_empty(&self) -> StorageBackendResult<bool> {
        Ok(self.len()? == 0)
    }

    fn check(&self, invoking: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        invoking.check()
    }

    fn node(&self, id: DocId, invoking: &StorageReadControl) -> StorageBackendResult<Option<Node>> {
        self.check(invoking)?;
        let selected = self.view.entry(&key(NODE, id), invoking).map_err(failure)?;
        self.check(invoking)?;
        let Some(entry) = selected else {
            return Ok(None);
        };
        let Some(length) = entry.value_len() else {
            return Ok(None);
        };
        if length != 35 {
            return Err(invalid("invalid ordered node length"));
        }
        let record = entry.read(invoking).map_err(failure)?;
        let node = Node::decode(
            record
                .value()
                .ok_or_else(|| invalid("missing ordered node"))?,
        )?;
        self.check(invoking)?;
        Ok(Some(node))
    }

    /// One physical metadata position, including a deletion. Callers account for deleted positions instead of hiding their traversal inside one live selection.
    pub fn next_position(
        &self,
        after: Option<DocId>,
        invoking: &StorageReadControl,
    ) -> StorageBackendResult<Option<(DocId, bool)>> {
        self.check(invoking)?;
        let after = after.map(|id| key(NODE, id));
        let mut cursor = self
            .view
            .cursor(&[NODE], after.as_ref().map(<[u8; 9]>::as_slice), invoking)
            .map_err(failure)?;
        let selected = cursor
            .next(invoking)
            .map_err(failure)?
            .map(|entry| Ok((decode_id(entry.key())?, entry.metadata().live)))
            .transpose();
        self.check(invoking)?;
        selected
    }

    /// Stream the original UQAVAL01 chunks without decoding the value. Preparation accounts for every chunk metadata position, including deleted tails, before any bytes are exposed.
    pub fn value_image_reader<'a>(
        &'a self,
        id: DocId,
        invoking: &StorageReadControl,
    ) -> StorageBackendResult<Option<ImageReader<'a>>> {
        let Some(node) = self.node(id, invoking)? else {
            return Ok(None);
        };
        image::reader(self, id, node.bytes, invoking).map(Some)
    }

    /// Decode only the requested value, retaining its independent invoking allowance. Absence and evaluated SQL NULL remain distinct.
    pub fn get(
        &self,
        id: DocId,
        invoking: &StorageReadControl,
    ) -> StorageBackendResult<Option<Budgeted<Value>>> {
        let Some(reader) = self.value_image_reader(id, invoking)? else {
            return Ok(None);
        };
        let mut input = Input {
            reader,
            error: None,
        };
        let value = ImageInput {
            reader: &mut input,
            control: invoking,
        }
        .value();
        if let Some(error) = input.error.take() {
            return Err(error);
        }
        let value = value?;
        let mut tail = [0];
        let end = input.read(&mut tail);
        if let Some(error) = input.error.take() {
            return Err(error);
        }
        if end.map_err(|error| StorageBackendError::backend("retained value image", error))? != 0 {
            return Err(invalid("trailing retained value image"));
        }
        self.check(invoking)?;
        Ok(Some(value))
    }

    pub fn visit_values_owned(
        &self,
        invoking: &StorageReadControl,
        visit: &mut dyn FnMut(DocId, Budgeted<Value>) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<()> {
        self.check(invoking)?;
        let mut cursor = self.view.cursor(&[NODE], None, invoking).map_err(failure)?;
        while let Some(entry) = cursor.next(invoking).map_err(failure)? {
            self.check(invoking)?;
            if !entry.metadata().live {
                continue;
            }
            let id = decode_id(entry.key())?;
            let value = self
                .get(id, invoking)?
                .ok_or_else(|| invalid("missing retained value"))?;
            if !visit(id, value)? {
                break;
            }
        }
        self.check(invoking)
    }
}

struct Input<'a> {
    reader: ImageReader<'a>,
    error: Option<StorageBackendError>,
}

impl Read for Input<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(output).map_err(|error| {
            self.error = Some(error);
            std::io::Error::other("retained value image read failed")
        })
    }
}

fn failure(error: VersionError) -> StorageBackendError {
    error.into_storage_error()
}

#[cfg(test)]
mod tests;
