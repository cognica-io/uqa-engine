//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-size node metadata and chunked exact values share the private-record lifecycle.

use crate::{
    mvcc::{PrivateRecordChanges, PrivateRecordSnapshot, RecordWrite},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use uqa_core::{memory::MemoryBudget, DocId, Value};

#[cfg(test)]
thread_local! {
    pub(super) static VALUE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static NODE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const CHUNK: usize = 4096;
const NODE: u8 = 0;
const VALUE: u8 = 1;
pub(super) const MAX_HEIGHT: u8 = 96;

#[derive(Clone, Copy)]
pub(super) struct Node {
    pub left: Option<DocId>,
    pub right: Option<DocId>,
    pub height: u8,
    pub bytes: u64,
    pub count: u64,
}

impl Node {
    fn encode(self) -> [u8; 35] {
        let mut bytes = [0; 35];
        bytes[0] = self.height;
        for (offset, child) in [(1, self.left), (10, self.right)] {
            bytes[offset] = u8::from(child.is_some());
            bytes[offset + 1..offset + 9].copy_from_slice(&child.unwrap_or(0).to_be_bytes());
        }
        bytes[19..27].copy_from_slice(&self.bytes.to_be_bytes());
        bytes[27..].copy_from_slice(&self.count.to_be_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 35
            || bytes[0] == 0
            || bytes[0] > MAX_HEIGHT
            || bytes[1] > 1
            || bytes[10] > 1
            || bytes[27..].iter().all(|byte| *byte == 0)
        {
            return Err(invalid("invalid ordered node"));
        }
        let child = |offset| {
            (bytes[offset] == 1).then(|| {
                u64::from_be_bytes(
                    bytes[offset + 1..offset + 9]
                        .try_into()
                        .expect("checked node"),
                )
            })
        };
        Ok(Self {
            left: child(1),
            right: child(10),
            height: bytes[0],
            bytes: u64::from_be_bytes(bytes[19..27].try_into().expect("checked node")),
            count: u64::from_be_bytes(bytes[27..].try_into().expect("checked node")),
        })
    }
}

pub(super) struct Records<'a> {
    changes: &'a PrivateRecordChanges,
    pub control: StorageReadControl,
}

impl<'a> Records<'a> {
    pub fn new(
        changes: &'a PrivateRecordChanges,
        memory: &MemoryBudget,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            changes,
            control: StorageReadControl::new(memory, control.cancellation()),
        }
    }
    pub fn snapshot(&self) -> StorageBackendResult<PrivateRecordSnapshot> {
        self.control.check()?;
        self.changes.snapshot().map_err(failure)
    }
    pub fn node(&self, id: DocId) -> StorageBackendResult<Node> {
        self.current_node(id)?
            .ok_or_else(|| invalid("missing ordered node"))
    }
    pub fn current_node(&self, id: DocId) -> StorageBackendResult<Option<Node>> {
        #[cfg(test)]
        NODE_READS.with(|reads| reads.set(reads.get() + 1));
        self.changes
            .get(&key(NODE, id), &self.control)
            .map_err(failure)?
            .as_ref()
            .and_then(|record| record.value())
            .map(Node::decode)
            .transpose()
    }
    pub fn node_at(
        &self,
        view: &PrivateRecordSnapshot,
        id: DocId,
    ) -> StorageBackendResult<Option<Node>> {
        #[cfg(test)]
        NODE_READS.with(|reads| reads.set(reads.get() + 1));
        view.get(&key(NODE, id), &self.control)
            .map_err(failure)?
            .as_ref()
            .and_then(|record| record.value())
            .map(Node::decode)
            .transpose()
    }
    pub fn set_node(&self, id: DocId, node: Node) -> StorageBackendResult<()> {
        self.write(&key(NODE, id), Some(&node.encode()))
    }
    pub fn height(&self, id: Option<DocId>) -> StorageBackendResult<u8> {
        id.map_or(Ok(0), |id| Ok(self.node(id)?.height))
    }
    pub fn count(&self, id: Option<DocId>) -> StorageBackendResult<u64> {
        id.map_or(Ok(0), |id| Ok(self.node(id)?.count))
    }
    pub fn read_scope(&self) -> StorageBackendResult<crate::mvcc::PrivateRecordReadScope> {
        self.changes.read_scope(&self.control).map_err(failure)
    }
    fn write(&self, key: &[u8], value: Option<&[u8]>) -> StorageBackendResult<()> {
        self.changes
            .apply(
                &[RecordWrite {
                    key,
                    expected: None,
                    value,
                }],
                &self.control,
            )
            .map_err(failure)
    }
    pub fn get(
        &self,
        view: &PrivateRecordSnapshot,
        id: DocId,
    ) -> StorageBackendResult<Option<Value>> {
        #[cfg(test)]
        VALUE_READS.with(|reads| reads.set(reads.get() + 1));
        let Some(node) = self.node_at(view, id)? else {
            return Ok(None);
        };
        let count = usize::try_from(node.bytes)
            .map_err(|_| invalid("value length outside address space"))?;
        let mut bytes = Vec::new();
        let mut cursor = view
            .cursor(&key(VALUE, id), None, &self.control)
            .map_err(failure)?;
        let mut ordinal = 0_u64;
        while let Some(entry) = cursor.next(&self.control).map_err(failure)? {
            if !entry.metadata().live {
                continue;
            }
            if entry.key() != chunk_key(id, ordinal) {
                return Err(invalid("unordered value chunk"));
            }
            let record = entry.read(&self.control).map_err(failure)?;
            let chunk = record
                .value()
                .ok_or_else(|| invalid("missing value chunk"))?;
            let remaining = count.saturating_sub(bytes.len());
            if chunk.len() != remaining.min(CHUNK) || remaining == 0 {
                return Err(invalid("invalid value chunk length"));
            }
            bytes.try_reserve(chunk.len()).map_err(|error| {
                StorageBackendError::backend("retained value allocation", error)
            })?;
            bytes.extend_from_slice(chunk);
            ordinal += 1;
        }
        if bytes.len() != count {
            return Err(invalid("truncated retained value"));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(StorageBackendError::from)
    }
    pub fn put_value(&self, id: DocId, value: &Value) -> StorageBackendResult<u64> {
        let bytes = serde_json::to_vec(value)?;
        for (ordinal, chunk) in bytes.chunks(CHUNK).enumerate() {
            self.control.check()?;
            self.write(&chunk_key(id, ordinal as u64), Some(chunk))?;
        }
        self.set_node(
            id,
            Node {
                left: None,
                right: None,
                height: 1,
                bytes: bytes.len() as u64,
                count: 1,
            },
        )?;
        Ok(bytes.len() as u64)
    }
    pub fn remove_value(&self, id: DocId) -> StorageBackendResult<()> {
        let node = self.node(id)?;
        for ordinal in 0..node.bytes.div_ceil(CHUNK as u64) {
            self.write(&chunk_key(id, ordinal), None)?;
        }
        self.write(&key(NODE, id), None)
    }
    pub fn visit_values(
        &self,
        view: &PrivateRecordSnapshot,
        visit: &mut dyn FnMut(DocId, Value) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<()> {
        let mut cursor = view.cursor(&[NODE], None, &self.control).map_err(failure)?;
        while let Some(entry) = cursor.next(&self.control).map_err(failure)? {
            if !entry.metadata().live {
                continue;
            }
            let id = decode_id(entry.key())?;
            let value = self
                .get(view, id)?
                .ok_or_else(|| invalid("missing retained value"))?;
            if !visit(id, value)? {
                break;
            }
        }
        Ok(())
    }
}

fn key(kind: u8, id: DocId) -> [u8; 9] {
    let mut key = [0; 9];
    key[0] = kind;
    key[1..].copy_from_slice(&id.to_be_bytes());
    key
}
fn chunk_key(id: DocId, ordinal: u64) -> [u8; 17] {
    let mut value = [0; 17];
    value[..9].copy_from_slice(&key(VALUE, id));
    value[9..].copy_from_slice(&ordinal.to_be_bytes());
    value
}
fn decode_id(key: &[u8]) -> StorageBackendResult<DocId> {
    if key.len() != 9 {
        return Err(invalid("invalid node identity"));
    }
    Ok(u64::from_be_bytes(
        key[1..].try_into().expect("checked identity"),
    ))
}
fn failure(error: crate::mvcc::VersionError) -> StorageBackendError {
    StorageBackendError::backend("retained value index", error)
}
pub(super) fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(format!("retained value index: {message}"))
}
