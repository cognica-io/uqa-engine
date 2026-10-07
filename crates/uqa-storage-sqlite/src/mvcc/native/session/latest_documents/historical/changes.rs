//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge historical replacements and captured private rows before physical rows.

use super::{
    CommitSequence, Connection, NativeRecordIdentity, PhysicalResult, PrivateRows, Result,
    StorageReadControl, ValueRef, VersionError,
};
use crate::mvcc::{native::decode_record, read};
use rusqlite::Rows;

pub(super) struct Changes<'a> {
    rows: Rows<'a>,
    connection: &'a Connection,
    documents: NativeRecordIdentity,
    boundary: CommitSequence,
    control: &'a StorageReadControl,
    private: Option<PrivateRows<'a>>,
    current: Option<i64>,
}

impl<'a> Changes<'a> {
    pub(super) fn new(
        rows: Rows<'a>,
        connection: &'a Connection,
        documents: NativeRecordIdentity,
        boundary: CommitSequence,
        control: &'a StorageReadControl,
        private: Option<PrivateRows<'a>>,
    ) -> PhysicalResult<Self> {
        let mut changes = Self {
            rows,
            connection,
            documents,
            boundary,
            control,
            private,
            current: None,
        };
        changes.advance()?;
        Ok(changes)
    }

    fn advance(&mut self) -> PhysicalResult<()> {
        self.current = None;
        self.control.check().map_err(VersionError::from)?;
        let Some(row) = self.rows.next()? else {
            return Ok(());
        };
        let key = row
            .get_ref(0)?
            .as_blob()
            .map_err(|_| VersionError::InvalidEncoding("invalid historical document key"))?;
        let identity =
            NativeRecordIdentity::visit_key_components(key, self.control, |position, value| {
                if position != 0 {
                    return Err(VersionError::InvalidEncoding(
                        "unexpected document key component",
                    ));
                }
                self.current = Some(value.as_i64().map_err(|_| {
                    VersionError::InvalidEncoding("native document key must be integer")
                })?);
                Ok(())
            })?;
        if identity != self.documents || self.current.is_none() {
            return Err(
                VersionError::InvalidEncoding("historical document key changed owner").into(),
            );
        }
        Ok(())
    }

    /// Emit changed rows up to `stored`, or all remaining rows when it is absent.
    /// The second result says that a changed identity masks the stored row.
    pub(super) fn before(
        &mut self,
        stored: Option<i64>,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> PhysicalResult<(bool, bool)> {
        let mut replaced = false;
        loop {
            self.control.check().map_err(VersionError::from)?;
            let private = self.private.as_ref().and_then(PrivateRows::peek);
            let next = match (private, self.current) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let Some(id) = next.filter(|id| stored.is_none_or(|stored| *id <= stored)) else {
                return Ok((true, replaced));
            };
            replaced |= stored == Some(id);
            let more = if private == Some(id) {
                self.private
                    .as_ref()
                    .expect("selected private row")
                    .visit(visit)?
                    .unwrap_or(true)
            } else {
                self.visit_historical(id, visit)?
            };
            if !more {
                return Ok((false, replaced));
            }
            if private == Some(id) {
                self.private
                    .as_mut()
                    .expect("selected private row")
                    .advance()?;
            }
            if self.current == Some(id) {
                self.advance()?;
            }
        }
    }

    fn visit_historical(
        &self,
        id: i64,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> PhysicalResult<bool> {
        let key = self
            .documents
            .encode_key(&[ValueRef::Integer(id)], self.control)?;
        let mut more = true;
        read::value(
            self.connection,
            &key,
            self.boundary,
            self.control,
            &mut |record| {
                if let Some(bytes) = record.and_then(|record| record.value) {
                    let (_, row) = decode_record(&key, bytes, self.control)?;
                    more = visit(&row).map_err(|error| VersionError::Storage(error.into()))?;
                }
                Ok(())
            },
        )?;
        Ok(more)
    }
}
