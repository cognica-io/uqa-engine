//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebuild in dependency order, then compare every physical row with its staged immutable record.

use super::{
    decode_record, decode_row, invalid, owners, physical, publish_row, read, BudgetedVec,
    CommitSequence, Connection, DatabaseId, Error, Family, NativeRecord, NativeRecordIdentity,
    NativeRecordOwner, PhysicalResult, StorageReadControl, VersionError, ORDER,
};
use crate::mvcc::admission;

enum Stage {
    Publish,
    Verify,
    Complete,
}

pub(in crate::mvcc::native) struct ImageProjection {
    namespace: DatabaseId,
    sequence: CommitSequence,
    cursor: Option<BudgetedVec<u8>>,
    family: usize,
    counts: [u64; ORDER.len()],
    verified: u64,
    previous_sequence: Option<[u8; 16]>,
    stage: Stage,
}

impl ImageProjection {
    pub(in crate::mvcc::native) fn new(namespace: DatabaseId, sequence: CommitSequence) -> Self {
        Self {
            namespace,
            sequence,
            cursor: None,
            family: 0,
            counts: [0; ORDER.len()],
            verified: 0,
            previous_sequence: None,
            stage: Stage::Publish,
        }
    }

    /// Each call visits at most one source or physical row, or advances one fixed family boundary.
    pub(in crate::mvcc::native) fn advance(
        &mut self,
        connection: &Connection,
        control: &StorageReadControl,
    ) -> PhysicalResult<bool> {
        control.check().map_err(VersionError::from)?;
        if matches!(self.stage, Stage::Complete) {
            return Ok(true);
        }
        let _permit = admission::permit(connection, control)?;
        let transaction = admission::begin(connection, control)?;
        let family = ORDER[self.family];
        match self.stage {
            Stage::Publish => self.publish_next(&transaction, family, control)?,
            Stage::Verify => self.verify_next(&transaction, family, control)?,
            Stage::Complete => unreachable!(),
        }
        admission::commit(transaction, control)?;
        Ok(matches!(self.stage, Stage::Complete))
    }

    fn publish_next(
        &mut self,
        transaction: &Connection,
        family: Family,
        control: &StorageReadControl,
    ) -> PhysicalResult<()> {
        let prefix = NativeRecordIdentity::family_prefix(family, control)?;
        let mut next = None;
        read::keys(
            transaction,
            &prefix,
            self.cursor.as_deref(),
            1,
            control,
            &mut |key| {
                let mut owned = BudgetedVec::new(control.memory());
                owned.extend_from_slice(key).map_err(VersionError::from)?;
                next = Some(owned);
                Ok(Some(false))
            },
        )?;
        if let Some(key) = next {
            read::value(transaction, &key, self.sequence, control, &mut |record| {
                (|| -> PhysicalResult<()> {
                    let bytes = record
                        .and_then(|row| row.value)
                        .ok_or_else(|| invalid("missing staged native image row"))?;
                    let (identity, values) = decode_record(&key, bytes, control)?;
                    if family == Family::Sequences {
                        let NativeRecordOwner::Object {
                            identity: object, ..
                        } = identity.owner()
                        else {
                            unreachable!()
                        };
                        if self.previous_sequence == Some(object) {
                            return Err(invalid("native sequences share an object identity").into());
                        }
                        self.previous_sequence = Some(object);
                    }
                    publish_row(transaction, family, &values, control)
                })()
                .map_err(Error::into_version)
            })?;
            self.counts[self.family] = self.counts[self.family]
                .checked_add(1)
                .ok_or_else(|| invalid("native image row count overflow"))?;
            self.cursor = Some(key);
        } else {
            self.next_family();
        }
        Ok(())
    }
    fn verify_next(
        &mut self,
        transaction: &Connection,
        family: Family,
        control: &StorageReadControl,
    ) -> PhysicalResult<()> {
        if let Some(row) = physical::next(
            transaction,
            family.layout(),
            self.cursor.as_deref(),
            control,
        )? {
            let values = decode_row(&row, family.layout().columns.len(), control)?;
            let owner = owners::for_row(transaction, self.namespace, family, &values, control)?;
            let identity = NativeRecordIdentity::new(family, owner)?;
            owners::validate(transaction, self.namespace, identity, &values, control)?;
            super::super::sequence_values::validate_image_pair(transaction, identity, control)?;
            super::super::backup::validation::validate_diskann(
                transaction,
                family,
                &values,
                control,
            )?;
            super::super::graph_lookup::validate_image_source(
                transaction,
                self.namespace,
                self.sequence,
                family,
                &values,
                control,
            )?;
            let record = NativeRecord::encode(family, owner, &values, control)?;
            read::value(
                transaction,
                record.key(),
                self.sequence,
                control,
                &mut |stored| {
                    if stored.and_then(|stored| stored.value) != Some(record.row()) {
                        return Err(invalid("native image physical row differs from its record"));
                    }
                    Ok(())
                },
            )?;
            self.cursor = Some(physical::physical_key(family.layout(), &values, control)?);
            self.verified = self
                .verified
                .checked_add(1)
                .ok_or_else(|| invalid("native image row count overflow"))?;
        } else {
            if self.verified != self.counts[self.family] {
                return Err(invalid("native image lost or duplicated physical rows").into());
            }
            self.next_family();
        }
        Ok(())
    }

    fn next_family(&mut self) {
        self.cursor = None;
        self.verified = 0;
        self.family += 1;
        if self.family == ORDER.len() {
            self.family = 0;
            self.stage = match self.stage {
                Stage::Publish => Stage::Verify,
                Stage::Verify | Stage::Complete => Stage::Complete,
            };
        }
    }
}
