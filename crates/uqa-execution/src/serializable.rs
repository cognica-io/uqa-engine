//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical relation and row observations selected by the executing access path.

use uqa_core::{CancellationToken, DocId};
use uqa_sql::SQLError;
use uqa_storage::{
    mvcc::{
        SerializableKeySpace, SerializablePredicate, SerializableReadContext, SerializableSession,
    },
    read_control::StorageReadControl,
};

use crate::storage_errors::storage_error;

/// A physical reader retains its original participant, immutable relation identity and shared allowance. It can neither admit another participant nor finish its transaction.
#[derive(Clone)]
pub struct SerializableRelationRead {
    object: [u8; 16],
    context: SerializableReadContext,
    control: StorageReadControl,
}

impl SerializableRelationRead {
    pub fn new(
        object: [u8; 16],
        context: SerializableReadContext,
        cancellation: &CancellationToken,
    ) -> Self {
        let control = context.read_control(cancellation);
        Self {
            object,
            context,
            control,
        }
    }

    /// A sequential access path observes the relation before reading even an empty result. Index access paths must supply their own precise logical ranges instead.
    pub fn observe_scan(&self) -> Result<(), SQLError> {
        self.observe(SerializablePredicate::object(self.object))
    }

    /// Rechecks observe the selected logical row even when it is absent or supplied from a retained cache.
    pub fn observe_row(&self, doc_id: DocId) -> Result<(), SQLError> {
        self.observe(SerializablePredicate::point(
            self.object,
            SerializableKeySpace::Rows,
            &doc_id.to_be_bytes(),
        ))
    }

    fn observe(&self, predicate: SerializablePredicate<'_>) -> Result<(), SQLError> {
        self.context
            .observe_read(predicate, &self.control)
            .map_err(|error| {
                storage_error("observe serializable read", &error.into_storage_error())
            })
    }
}

/// Demand-driven sources retain one relation observation for a sequential scan and precise row observations for selected tuple rechecks.
#[derive(Default)]
pub struct SerializableScan {
    read: Option<SerializableRelationRead>,
    relation_observed: bool,
}

impl SerializableScan {
    pub fn new(read: Option<SerializableRelationRead>) -> Self {
        Self {
            read,
            relation_observed: false,
        }
    }

    pub fn observe_relation(&mut self) -> Result<(), SQLError> {
        if !self.relation_observed {
            if let Some(read) = &self.read {
                read.observe_scan()?;
            }
            self.relation_observed = true;
        }
        Ok(())
    }

    pub fn observe_row(&self, doc_id: DocId) -> Result<(), SQLError> {
        if let Some(read) = &self.read {
            read.observe_row(doc_id)?;
        }
        Ok(())
    }
}

/// State adapters supply the original mutation session and a persistent relation's immutable identity. Temporary relations do not participate in shared conflict tracking.
pub trait SerializableWrites {
    fn serializable_session(&self) -> Option<&dyn SerializableSession>;
    fn serializable_write_object(&self, table: &str) -> Result<Option<[u8; 16]>, SQLError>;
}

/// Record evaluated row intent before its private mutation. The surrounding statement/savepoint owns rollback of both the intent and the row change.
pub fn observe_row_write(
    writes: &dyn SerializableWrites,
    table: &str,
    doc_id: DocId,
) -> Result<(), SQLError> {
    let Some(session) = writes.serializable_session() else {
        return Ok(());
    };
    if session
        .serializable_read_context()
        .map_err(|error| storage_error("inspect serializable writer", &error))?
        .is_none()
    {
        return Ok(());
    }
    let Some(object) = writes.serializable_write_object(table)? else {
        return Ok(());
    };
    session
        .observe_serializable_write(SerializablePredicate::point(
            object,
            SerializableKeySpace::Rows,
            &doc_id.to_be_bytes(),
        ))
        .map_err(|error| storage_error("observe serializable write", &error))
}
