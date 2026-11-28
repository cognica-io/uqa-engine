//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A staged value is hashed once, under the writer's control, while its version metadata is inserted.

use super::{CaptureScope, ConnectionFunctions};
use crate::mvcc::{Error, PhysicalResult};
use rusqlite::{functions::Context, types::ValueRef, Connection};
use std::sync::Arc;
use uqa_storage::{
    backup::RecordImageHasher, mvcc::CommitSequence, read_control::StorageReadControl,
};

impl ConnectionFunctions {
    pub(super) fn image_digest(&self, context: &Context<'_>) -> rusqlite::Result<Option<[u8; 32]>> {
        let mut active = self.fingerprints.lock();
        let Some(scope) = active.as_mut() else {
            // Historical copies have no fingerprint. A streamed backup value supplies its fingerprint only after the final BLOB write.
            return Ok(None);
        };
        let ValueRef::Blob(value) = context.get_raw(1) else {
            return Ok(None);
        };
        let revision = CommitSequence::from_u64(u64::from_be_bytes(context.get::<[u8; 8]>(0)?));
        match RecordImageHasher::for_value(revision, value, &scope.control) {
            Ok(digest) => Ok(Some(digest.as_bytes())),
            Err(error) => {
                scope.error.get_or_insert(error);
                Err(rusqlite::Error::UserFunctionError(Box::new(
                    std::io::Error::other("record image hashing failed"),
                )))
            }
        }
    }
}

pub(in crate::mvcc) struct FingerprintScope {
    functions: Arc<ConnectionFunctions>,
    previous: Option<CaptureScope>,
}

impl FingerprintScope {
    pub(in crate::mvcc) fn new(
        connection: &Connection,
        control: &StorageReadControl,
    ) -> PhysicalResult<Self> {
        let functions = ConnectionFunctions::of(connection)?;
        let previous = functions.fingerprints.lock().replace(CaptureScope {
            control: control.clone(),
            error: None,
        });
        Ok(Self {
            functions,
            previous,
        })
    }

    pub(in crate::mvcc) fn resolve(&self, result: rusqlite::Result<usize>) -> PhysicalResult<()> {
        if let Err(error) = result {
            if let Some(error) = self
                .functions
                .fingerprints
                .lock()
                .as_mut()
                .and_then(|scope| scope.error.take())
            {
                return Err(Error::Version(error));
            }
            return Err(error.into());
        }
        Ok(())
    }
}

impl Drop for FingerprintScope {
    fn drop(&mut self) {
        *self.functions.fingerprints.lock() = self.previous.take();
    }
}
