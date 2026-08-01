//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Budgeted DISTINCT tracking with disk fallback.

use super::{
    read_bounded_json_spill_record, BTreeSet, BufReader, BufWriter, DecimalValue, SQLError, Seek,
    SeekFrom, Value, Write,
};

pub(in crate::sql) struct DistinctTracker {
    pub(super) memory: BTreeSet<String>,
    pub(super) memory_bytes: usize,
    pub(super) max_memory_record_bytes: usize,
    pub(super) budget_bytes: usize,
    pub(super) disk: Option<tempfile::NamedTempFile>,
    pub(super) max_disk_record_bytes: usize,
}

impl Default for DistinctTracker {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024)
    }
}

impl DistinctTracker {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            memory: BTreeSet::new(),
            memory_bytes: 0,
            max_memory_record_bytes: 0,
            budget_bytes: budget_bytes.max(1),
            disk: None,
            max_disk_record_bytes: 0,
        }
    }

    pub(super) fn insert(&mut self, key: String) -> Result<bool, SQLError> {
        if self.memory.contains(&key) || self.disk_contains(&key)? {
            return Ok(false);
        }
        let encoded_bytes = serde_json::to_vec(&key)
            .map_err(|error| {
                SQLError::Internal(format!("failed to size aggregate DISTINCT key: {error}"))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate DISTINCT size overflow".into()))?;
        self.memory_bytes = self
            .memory_bytes
            .checked_add(encoded_bytes)
            .ok_or_else(|| SQLError::Internal("aggregate DISTINCT size overflow".into()))?;
        self.max_memory_record_bytes = self.max_memory_record_bytes.max(encoded_bytes);
        self.memory.insert(key);
        if self.memory_bytes > self.budget_bytes {
            self.spill()?;
        }
        Ok(true)
    }

    pub(super) fn disk_contains(&self, wanted: &str) -> Result<bool, SQLError> {
        let Some(file) = self.disk.as_ref() else {
            return Ok(false);
        };
        let file = file.reopen().map_err(|error| {
            SQLError::Internal(format!(
                "failed to reopen aggregate DISTINCT spill: {error}"
            ))
        })?;
        let mut reader = BufReader::new(file);
        loop {
            let Some(record) = read_bounded_json_spill_record(
                &mut reader,
                self.max_disk_record_bytes,
                "aggregate DISTINCT spill row",
            )?
            else {
                return Ok(false);
            };
            let key: String = serde_json::from_slice(&record).map_err(|error| {
                SQLError::Internal(format!(
                    "failed to decode aggregate DISTINCT spill: {error}"
                ))
            })?;
            if key == wanted {
                return Ok(true);
            }
        }
    }

    pub(super) fn spill(&mut self) -> Result<(), SQLError> {
        if self.memory.is_empty() {
            return Ok(());
        }
        if self.disk.is_none() {
            self.disk = Some(tempfile::NamedTempFile::new().map_err(|error| {
                SQLError::Internal(format!(
                    "failed to create aggregate DISTINCT spill: {error}"
                ))
            })?);
        }
        let file = self.disk.as_mut().ok_or_else(|| {
            SQLError::Internal("aggregate DISTINCT spill file was not initialized".into())
        })?;
        let original_length = file.as_file_mut().seek(SeekFrom::End(0)).map_err(|error| {
            SQLError::Internal(format!("failed to seek aggregate DISTINCT spill: {error}"))
        })?;
        let next_max_disk_record_bytes =
            self.max_disk_record_bytes.max(self.max_memory_record_bytes);
        let result = {
            let mut writer = BufWriter::new(file.as_file_mut());
            let result = (|| -> Result<(), SQLError> {
                for key in &self.memory {
                    serde_json::to_writer(&mut writer, key).map_err(|error| {
                        SQLError::Internal(format!(
                            "failed to encode aggregate DISTINCT key: {error}"
                        ))
                    })?;
                    writer.write_all(b"\n").map_err(|error| {
                        SQLError::Internal(format!(
                            "failed to write aggregate DISTINCT key: {error}"
                        ))
                    })?;
                }
                writer.flush().map_err(|error| {
                    SQLError::Internal(format!("failed to flush aggregate DISTINCT spill: {error}"))
                })
            })();
            drop(writer);
            result
        };
        if let Err(error) = result {
            file.as_file_mut()
                .set_len(original_length)
                .map_err(|rollback| {
                    SQLError::Internal(format!(
                        "{error}; failed to roll back aggregate DISTINCT spill: {rollback}"
                    ))
                })?;
            return Err(error);
        }
        self.memory.clear();
        self.memory_bytes = 0;
        self.max_memory_record_bytes = 0;
        self.max_disk_record_bytes = next_max_disk_record_bytes;
        Ok(())
    }
}

pub(in crate::sql) fn distinct_key(v: &Value) -> Result<String, SQLError> {
    Ok(match v {
        Value::Null => "\x00".into(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Int(n) => format!("i:{n}"),
        Value::Float(f) => format!("f:{:016x}", f.to_bits()),
        Value::Decimal(d) => format!("n:{}", d.to_canonical_string()),
        Value::Str(s) => format!("s:{s}"),
        Value::Bytes(bytes) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let capacity = bytes
                .len()
                .checked_mul(2)
                .and_then(|length| length.checked_add(2))
                .ok_or_else(|| SQLError::Internal("aggregate DISTINCT key size overflow".into()))?;
            let mut key = String::new();
            key.try_reserve_exact(capacity).map_err(|error| {
                SQLError::Internal(format!(
                    "unable to allocate aggregate DISTINCT key of {capacity} bytes: {error}"
                ))
            })?;
            key.push_str("y:");
            for byte in bytes {
                key.push(char::from(HEX[usize::from(byte >> 4)]));
                key.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
            key
        }
        Value::Temporal(t) => format!("t:{}", t.to_sql_string()),
        other => format!("o:{other:?}"),
    })
}

pub(in crate::sql) fn value_as_f64(v: &Value) -> Result<f64, SQLError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("expected number that fits float, got {v:?}"))
        }),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

pub(in crate::sql) fn value_lt(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x < y,
        (Value::Float(x), Value::Float(y)) => x < y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) < *y,
        (Value::Float(x), Value::Int(y)) => *x < (*y as f64),
        (Value::Decimal(x), Value::Decimal(y)) => x < y,
        (Value::Int(x), Value::Decimal(y)) => DecimalValue::from_i64(*x) < *y,
        (Value::Decimal(x), Value::Int(y)) => *x < DecimalValue::from_i64(*y),
        (Value::Float(x), Value::Decimal(y)) => {
            DecimalValue::from_f64_lossy(*x).is_some_and(|x| x < *y)
        }
        (Value::Decimal(x), Value::Float(y)) => {
            DecimalValue::from_f64_lossy(*y).is_some_and(|y| *x < y)
        }
        (Value::Str(x), Value::Str(y)) => x < y,
        (Value::Temporal(x), Value::Temporal(y)) => x < y,
        _ => false,
    }
}

pub(in crate::sql) fn value_gt(a: &Value, b: &Value) -> bool {
    value_lt(b, a)
}
