//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence definitions are transactional catalog records keyed by name. The value state of each definition generation is a record of its own keyed by object identity and generation, which value operations move outside the transactions that change the definition, as `PostgreSQL` keeps a sequence's state in its relation's data rather than in `pg_sequence`.

use super::{
    decode_relation_key, decode_value, encode_value, relation_key, relations, single_str_key,
    KeyValueCatalog, RelationIdentity, RelationKind, SequenceOptions, SequenceReservationResult,
    SequenceRow, SequenceSetValueResult, StorageBackendError, StorageBackendResult, StoredSequence,
    StoredSequenceValue, TAG_RELATION, TAG_SCHEMA, TAG_SEQUENCE, TAG_SEQUENCE_VALUE,
};
use crate::catalog::{sequence_value_reservation, SequenceLogResult, SequenceValuePosition};
use crate::key_value::index_view::{evaluate_mutation, read_view};
use crate::key_value::KeyValueRead;

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

fn sequence_bounds(increment: i64, options: &SequenceOptions) -> (i64, i64) {
    let default_min = if increment > 0 { 1 } else { i64::MIN };
    let default_max = if increment > 0 { i64::MAX } else { -1 };
    (
        options.min_value.unwrap_or(default_min),
        options.max_value.unwrap_or(default_max),
    )
}

fn sequence_value_prefix(object_id: [u8; 16]) -> Vec<u8> {
    let mut key = Vec::with_capacity(33);
    key.push(TAG_SEQUENCE_VALUE);
    key.extend_from_slice(&object_id);
    key
}

fn sequence_value_key(object_id: [u8; 16], definition_generation: [u8; 16]) -> Vec<u8> {
    let mut key = sequence_value_prefix(object_id);
    key.extend_from_slice(&definition_generation);
    key
}

fn sequence_value_record(
    increment: i64,
    options: &SequenceOptions,
    position: SequenceValuePosition,
) -> StoredSequenceValue {
    let (min_value, max_value) = sequence_bounds(increment, options);
    StoredSequenceValue {
        current: position.current,
        called: position.called,
        log_count: position.log_count,
        increment,
        min_value,
        max_value,
        cycle: options.cycle,
        cache_size: options.cache_size,
    }
}

const fn row_position(sequence: &SequenceRow) -> SequenceValuePosition {
    SequenceValuePosition {
        current: sequence.current,
        called: sequence.called,
        log_count: sequence.log_count,
    }
}

/// The value record of one generation, or why there is none: another generation replaced it, or the sequence is gone.
enum GenerationValue {
    Found(StoredSequenceValue),
    Replaced,
    Missing,
}

fn generation_value(
    read: &dyn KeyValueRead,
    object_id: [u8; 16],
    definition_generation: [u8; 16],
) -> StorageBackendResult<GenerationValue> {
    if let Some(encoded) = read.get(&sequence_value_key(object_id, definition_generation))? {
        return Ok(GenerationValue::Found(decode_value(&encoded)?));
    }
    let mut replaced = false;
    read.visit_prefix(&sequence_value_prefix(object_id), &mut |_, _| {
        replaced = true;
        Ok(())
    })?;
    Ok(if replaced {
        GenerationValue::Replaced
    } else {
        GenerationValue::Missing
    })
}

/// The value state of a stored definition: its generation's value record, or the definition itself for one an earlier release wrote before the initial open moved its value state.
fn stored_position(
    read: &dyn KeyValueRead,
    relation: &RelationIdentity,
    stored: &StoredSequence,
) -> StorageBackendResult<SequenceValuePosition> {
    let record = read
        .get(&sequence_value_key(
            stored.object_id,
            stored.definition_generation,
        ))?
        .map(|encoded| decode_value::<StoredSequenceValue>(&encoded))
        .transpose()?;
    match (record, stored.legacy_value()) {
        (Some(record), None) => Ok(record.position()),
        (None, Some(legacy)) => Ok(legacy),
        (Some(_), Some(_)) => Err(StorageBackendError::Other(format!(
            "sequence `{}` has a definition written by an earlier release after its value record was created",
            relation.qualified_name()
        ))),
        (None, None) => Err(StorageBackendError::Other(format!(
            "sequence `{}` has no value record",
            relation.qualified_name()
        ))),
    }
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
        security: sequence.security.clone(),
        object_id: sequence.object_id,
        definition_generation: sequence.definition_generation,
        start: sequence.start,
        increment: sequence.increment,
        current: None,
        called: None,
        log_count: None,
        persistence: sequence.persistence.clone(),
        owner: sequence.owner,
        options: concrete_sequence_options(sequence),
    }
}

impl KeyValueCatalog {
    pub(super) fn sequence_has_private_changes_impl(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<bool> {
        if !self.store.transaction_model().is_versioned() {
            return Ok(false);
        }
        let key = relation_key(TAG_SEQUENCE, relation)?;
        read_view(self.store.as_ref(), |read| {
            Ok(read.revision(&[&key])?.has_private_changes())
        })
    }

    pub(super) fn sequence_value_has_private_changes_impl(
        &self,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<bool> {
        if !self.store.transaction_model().is_versioned() {
            return Ok(false);
        }
        let key = sequence_value_key(object_id, definition_generation);
        read_view(self.store.as_ref(), |read| {
            Ok(read.revision(&[&key])?.has_private_changes())
        })
    }

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
            let stored = stored_sequence(sequence);
            batch.put(&key, &encode_value(&stored)?)?;
            batch.put(
                &sequence_value_key(sequence.object_id, sequence.definition_generation),
                &encode_value(&sequence_value_record(
                    stored.increment,
                    &stored.options,
                    row_position(sequence),
                ))?,
            )?;
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
            let Some(encoded) = read.get(&key)? else {
                return Ok(false);
            };
            let previous: StoredSequence = decode_value(&encoded)?;
            let mut replacement = stored_sequence(sequence);
            if let Some(legacy) = previous.legacy_value() {
                // A definition an earlier release wrote keeps its value state until the value migration of the initial open moves it. That migration follows the identity migration, which gives such definitions their object identities and generations through this replacement.
                replacement.current = Some(legacy.current);
                replacement.called = Some(legacy.called);
                replacement.log_count = Some(legacy.log_count);
                batch.put(&key, &encode_value(&replacement)?)?;
                return Ok(true);
            }
            // The generation's value record moves only with value operations; a new generation starts a record of its own.
            if previous.object_id != replacement.object_id
                || previous.definition_generation != replacement.definition_generation
            {
                batch.delete(&sequence_value_key(
                    previous.object_id,
                    previous.definition_generation,
                ))?;
                batch.put(
                    &sequence_value_key(replacement.object_id, replacement.definition_generation),
                    &encode_value(&sequence_value_record(
                        replacement.increment,
                        &replacement.options,
                        row_position(sequence),
                    ))?,
                )?;
            }
            batch.put(&key, &encode_value(&replacement)?)?;
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
            let Some(encoded) = read.get(&key)? else {
                return Ok(false);
            };
            let stored: StoredSequence = decode_value(&encoded)?;
            batch.delete(&key)?;
            batch.delete(&sequence_value_key(
                stored.object_id,
                stored.definition_generation,
            ))?;
            relations::release_relation(read, batch, &relation, RelationKind::Sequence)?;
            Ok(true)
        })
    }

    pub(super) fn load_sequence_rows_impl(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        read_view(self.store.as_ref(), |read| {
            let mut rows = Vec::new();
            let mut definitions = Vec::new();
            read.visit_prefix(&[TAG_SEQUENCE], &mut |key, value| {
                definitions.push((
                    decode_relation_key(key)?,
                    decode_value::<StoredSequence>(value)?,
                ));
                Ok(())
            })?;
            for (relation, stored) in definitions {
                let position = stored_position(read, &relation, &stored)?;
                rows.push(SequenceRow {
                    relation,
                    security: stored.security,
                    object_id: stored.object_id,
                    definition_generation: stored.definition_generation,
                    start: stored.start,
                    increment: stored.increment,
                    current: position.current,
                    called: position.called,
                    log_count: position.log_count,
                    persistence: stored.persistence,
                    owner: stored.owner,
                    options: stored.options,
                });
            }
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
        let key = sequence_value_key(object_id, definition_generation);
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let mut stored = match generation_value(read, object_id, definition_generation)? {
                GenerationValue::Found(stored) => stored,
                GenerationValue::Replaced => {
                    return Ok(SequenceReservationResult::DefinitionChanged)
                }
                GenerationValue::Missing => return Ok(SequenceReservationResult::Missing),
            };
            if stored.increment == 0 || stored.cache_size <= 0 || stored.log_count < 0 {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` has increment {}, cache size {}, and log count {}",
                    stored.increment, stored.cache_size, stored.log_count
                )));
            }
            let Some(reservation) = sequence_value_reservation(
                stored.position(),
                stored.increment,
                stored.min_value,
                stored.max_value,
                stored.cycle,
                stored.cache_size,
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
        _name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> StorageBackendResult<SequenceSetValueResult> {
        if log_count < 0 {
            return Err(StorageBackendError::Other(
                "sequence log count cannot be negative".into(),
            ));
        }
        let key = sequence_value_key(object_id, definition_generation);
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let mut stored = match generation_value(read, object_id, definition_generation)? {
                GenerationValue::Found(stored) => stored,
                GenerationValue::Replaced => return Ok(SequenceSetValueResult::DefinitionChanged),
                GenerationValue::Missing => return Ok(SequenceSetValueResult::Missing),
            };
            stored.current = value;
            stored.called = called;
            stored.log_count = log_count;
            batch.put(&key, &encode_value(&stored)?)?;
            Ok(SequenceSetValueResult::Set(value))
        })
    }

    pub(super) fn log_sequence_values_impl(
        &self,
        _name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        expected: (i64, bool),
        logged: SequenceValuePosition,
    ) -> StorageBackendResult<SequenceLogResult> {
        if logged.log_count < 0 {
            return Err(StorageBackendError::Other(
                "sequence log count cannot be negative".into(),
            ));
        }
        let key = sequence_value_key(object_id, definition_generation);
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let mut stored = match generation_value(read, object_id, definition_generation)? {
                GenerationValue::Found(stored) => stored,
                GenerationValue::Replaced => return Ok(SequenceLogResult::DefinitionChanged),
                GenerationValue::Missing => return Ok(SequenceLogResult::Missing),
            };
            if (stored.current, stored.called) != expected {
                return Ok(SequenceLogResult::Changed(stored.position()));
            }
            stored.current = logged.current;
            stored.called = logged.called;
            stored.log_count = logged.log_count;
            batch.put(&key, &encode_value(&stored)?)?;
            Ok(SequenceLogResult::Logged)
        })
    }

    /// Give every definition an earlier release wrote, whose value state it kept itself, a value record holding that state, and rewrite the definition without it, in one evaluated mutation.
    pub(super) fn migrate_sequence_values_impl(&self) -> StorageBackendResult<()> {
        evaluate_mutation(self.store.as_ref(), |read, batch| {
            let mut legacy = Vec::new();
            read.visit_prefix(&[TAG_SEQUENCE], &mut |key, value| {
                let stored: StoredSequence = decode_value(value)?;
                if stored.legacy_value().is_some() {
                    legacy.push((key.to_vec(), stored));
                }
                Ok(())
            })?;
            for (key, mut stored) in legacy {
                let position = stored
                    .legacy_value()
                    .expect("selected definitions keep their value state");
                let value_key = sequence_value_key(stored.object_id, stored.definition_generation);
                if read.get(&value_key)?.is_some() {
                    return Err(StorageBackendError::Other(format!(
                        "sequence `{}` has a definition written by an earlier release after its value record was created",
                        decode_relation_key(&key)?.qualified_name()
                    )));
                }
                batch.put(
                    &value_key,
                    &encode_value(&sequence_value_record(
                        stored.increment,
                        &stored.options,
                        position,
                    ))?,
                )?;
                stored.current = None;
                stored.called = None;
                stored.log_count = None;
                batch.put(&key, &encode_value(&stored)?)?;
            }
            Ok(())
        })
    }
}
