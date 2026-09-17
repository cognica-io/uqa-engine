//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence definitions and value reservations use one evaluated catalog boundary.

use super::{
    decode_relation_key, decode_value, encode_value, relation_key, relations, single_str_key,
    KeyValueCatalog, RelationIdentity, RelationKind, SequenceOptions, SequenceReservationResult,
    SequenceRow, SequenceSetValueResult, StorageBackendError, StorageBackendResult, StoredSequence,
    TAG_RELATION, TAG_SCHEMA, TAG_SEQUENCE,
};
use crate::catalog::{sequence_value_reservation, SequenceValuePosition};
use crate::key_value::index_view::{evaluate_mutation, read_view};

fn concrete_sequence_options(sequence: &SequenceRow) -> SequenceOptions {
    let default_min = if sequence.increment > 0 { 1 } else { i64::MIN };
    let default_max = if sequence.increment > 0 { i64::MAX } else { -1 };
    SequenceOptions {
        data_type: sequence.options.data_type.clone(),
        min_value: Some(sequence.options.min_value.unwrap_or(default_min)),
        max_value: Some(sequence.options.max_value.unwrap_or(default_max)),
        cycle: sequence.options.cycle,
        cache_size: sequence.options.cache_size,
    }
}

fn sequence_bounds(stored: &StoredSequence) -> (i64, i64) {
    let default_min = if stored.increment > 0 { 1 } else { i64::MIN };
    let default_max = if stored.increment > 0 { i64::MAX } else { -1 };
    (
        stored.options.min_value.unwrap_or(default_min),
        stored.options.max_value.unwrap_or(default_max),
    )
}

fn validate_sequence_persistence(code: &str) -> StorageBackendResult<()> {
    if matches!(code, "p" | "u") {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "invalid durable sequence persistence `{code}`"
        )))
    }
}

fn stored_sequence(sequence: &SequenceRow) -> StoredSequence {
    StoredSequence {
        role_owner: sequence.role_owner.clone(),
        acl: sequence.acl.clone(),
        object_id: sequence.object_id,
        definition_generation: sequence.definition_generation,
        start: sequence.start,
        increment: sequence.increment,
        current: sequence.current,
        called: sequence.called,
        log_count: sequence.log_count,
        persistence: sequence.persistence.clone(),
        owner: sequence.owner,
        options: concrete_sequence_options(sequence),
    }
}

impl KeyValueCatalog {
    pub(super) fn create_sequence_row_impl(
        &self,
        sequence: &SequenceRow,
    ) -> StorageBackendResult<bool> {
        validate_sequence_persistence(&sequence.persistence)?;
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        let durable = self.store.identifier_allocator().is_some();
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            relations::require_schema_exists(read, &sequence.relation)?;
            if read.get(&key)?.is_some() {
                return Ok(false);
            }
            relations::claim_relation(read, batch, &sequence.relation, RelationKind::Sequence)?;
            if durable {
                batch.require_unchanged(&single_str_key(TAG_SCHEMA, &sequence.relation.schema)?)?;
            }
            batch.put(&key, &encode_value(&stored_sequence(sequence))?)?;
            Ok(true)
        })
    }

    pub(super) fn replace_sequence_row_impl(
        &self,
        sequence: &SequenceRow,
    ) -> StorageBackendResult<bool> {
        validate_sequence_persistence(&sequence.persistence)?;
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            if read.get(&key)?.is_none() {
                return Ok(false);
            }
            batch.put(&key, &encode_value(&stored_sequence(sequence))?)?;
            Ok(true)
        })
    }

    pub(super) fn rename_sequence_row_impl(
        &self,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        let from_relation =
            RelationIdentity::from_legacy_name(from).map_err(StorageBackendError::Other)?;
        let to_relation =
            RelationIdentity::from_legacy_name(to).map_err(StorageBackendError::Other)?;
        let from_key = relation_key(TAG_SEQUENCE, &from_relation)?;
        let to_key = relation_key(TAG_SEQUENCE, &to_relation)?;
        let to_relation_key = relation_key(TAG_RELATION, &to_relation)?;
        let durable = self.store.identifier_allocator().is_some();
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let Some(value) = read.get(&from_key)? else {
                return Ok(false);
            };
            if from_relation == to_relation {
                return Ok(true);
            }
            relations::require_schema_exists(read, &to_relation)?;
            if read.get(&to_key)?.is_some() || read.get(&to_relation_key)?.is_some() {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists",
                    to_relation.qualified_name()
                )));
            }
            relations::claim_relation(read, batch, &to_relation, RelationKind::Sequence)?;
            if durable {
                batch.require_unchanged(&single_str_key(TAG_SCHEMA, &to_relation.schema)?)?;
            }
            batch.put(&to_key, &value)?;
            batch.delete(&from_key)?;
            relations::release_relation(read, batch, &from_relation, RelationKind::Sequence)?;
            Ok(true)
        })
    }

    pub(super) fn drop_sequence_row_impl(&self, name: &str) -> StorageBackendResult<bool> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            if read.get(&key)?.is_none() {
                return Ok(false);
            }
            batch.delete(&key)?;
            relations::release_relation(read, batch, &relation, RelationKind::Sequence)?;
            Ok(true)
        })
    }

    pub(super) fn load_sequence_rows_impl(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        read_view(self.store.as_ref(), |read| {
            let mut rows = Vec::new();
            read.visit_prefix(&[TAG_SEQUENCE], &mut |key, value| {
                let relation = decode_relation_key(key)?;
                let stored: StoredSequence = decode_value(value)?;
                rows.push(SequenceRow {
                    relation,
                    role_owner: stored.role_owner,
                    acl: stored.acl,
                    object_id: stored.object_id,
                    definition_generation: stored.definition_generation,
                    start: stored.start,
                    increment: stored.increment,
                    current: stored.current,
                    called: stored.called,
                    log_count: stored.log_count,
                    persistence: stored.persistence,
                    owner: stored.owner,
                    options: stored.options,
                });
                Ok(())
            })?;
            rows.sort_by(|left, right| left.relation.cmp(&right.relation));
            Ok(rows)
        })
    }

    pub(super) fn reserve_sequence_values_impl(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<SequenceReservationResult> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let Some(value) = read.get(&key)? else {
                return Ok(SequenceReservationResult::Missing);
            };
            let mut stored: StoredSequence = decode_value(&value)?;
            if stored.object_id != object_id {
                return Ok(SequenceReservationResult::Missing);
            }
            if stored.definition_generation != definition_generation {
                return Ok(SequenceReservationResult::DefinitionChanged);
            }
            let (min_value, max_value) = sequence_bounds(&stored);
            if stored.increment == 0 || stored.options.cache_size <= 0 || stored.log_count < 0 {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` has increment {}, cache size {}, and log count {}",
                    stored.increment, stored.options.cache_size, stored.log_count
                )));
            }
            let Some(reservation) = sequence_value_reservation(
                SequenceValuePosition {
                    current: stored.current,
                    called: stored.called,
                    log_count: stored.log_count,
                },
                stored.increment,
                min_value,
                max_value,
                stored.options.cycle,
                stored.options.cache_size,
            ) else {
                return Ok(SequenceReservationResult::Exhausted);
            };
            stored.current = reservation.last_value;
            stored.called = true;
            stored.log_count = reservation.log_count;
            batch.put(&key, &encode_value(&stored)?)?;
            Ok(SequenceReservationResult::Reserved(reservation))
        })
    }

    pub(super) fn set_sequence_value_impl(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> StorageBackendResult<SequenceSetValueResult> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let Some(encoded) = read.get(&key)? else {
                return Ok(SequenceSetValueResult::Missing);
            };
            let mut stored: StoredSequence = decode_value(&encoded)?;
            if stored.object_id != object_id {
                return Ok(SequenceSetValueResult::Missing);
            }
            if stored.definition_generation != definition_generation {
                return Ok(SequenceSetValueResult::DefinitionChanged);
            }
            if log_count < 0 {
                return Err(StorageBackendError::Other(
                    "sequence log count cannot be negative".into(),
                ));
            }
            stored.current = value;
            stored.called = called;
            stored.log_count = log_count;
            batch.put(&key, &encode_value(&stored)?)?;
            Ok(SequenceSetValueResult::Set(value))
        })
    }
}
