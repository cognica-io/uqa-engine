//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Batch, BufRead, BufReader, BufWriter, DecimalValue, Deserialize, ExecError,
    ExecResult, File, NamedTempFile, Read, ResultRow, RowSchema, Seek, SeekFrom, Serialize,
    SerializeMap, SerializeSeq, Serializer, TemporalValue, Value, Write, SPILL_MAGIC,
};

#[derive(Default)]
pub(super) struct ByteCounter {
    pub(super) bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(buffer.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::FileTooLarge, "encoded size overflow")
        })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn spill_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}

pub(super) fn read_bounded_spill_record<R: BufRead>(
    reader: &mut R,
    max_record_bytes: usize,
    description: &str,
) -> ExecResult<Option<Vec<u8>>> {
    let mut record = Vec::new();
    loop {
        let (chunk_len, terminated) = {
            let available = reader
                .fill_buf()
                .map_err(|error| spill_error(format!("failed to read {description}: {error}")))?;
            if available.is_empty() {
                if record.is_empty() {
                    return Ok(None);
                }
                return Err(spill_error(format!(
                    "truncated {description}: missing record delimiter"
                )));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (available.len(), false),
            }
        };

        let next_len = record
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| spill_error(format!("{description} length overflow")))?;
        if next_len > max_record_bytes {
            return Err(spill_error(format!(
                "{description} exceeds recorded maximum of {max_record_bytes} bytes"
            )));
        }
        record.try_reserve(chunk_len).map_err(|error| {
            spill_error(format!(
                "unable to allocate {chunk_len} more bytes for {description}: {error}"
            ))
        })?;
        let available = reader
            .fill_buf()
            .map_err(|error| spill_error(format!("failed to read {description}: {error}")))?;
        record.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);

        if terminated {
            let delimiter = record.pop();
            debug_assert_eq!(delimiter, Some(b'\n'));
            return Ok(Some(record));
        }
    }
}

pub(super) fn open_spill_reader(file: &NamedTempFile) -> ExecResult<BufReader<File>> {
    let reopened = file
        .reopen()
        .map_err(|error| spill_error(format!("failed to reopen spill file: {error}")))?;
    let mut reader = BufReader::new(reopened);
    let mut magic = [0_u8; SPILL_MAGIC.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|error| spill_error(format!("failed to read spill header: {error}")))?;
    if magic != SPILL_MAGIC {
        return Err(spill_error("invalid spill file header"));
    }
    Ok(reader)
}

pub(super) fn append_batches(file: &mut File, batches: &[Batch]) -> ExecResult<()> {
    let original_len = file
        .seek(SeekFrom::End(0))
        .map_err(|error| spill_error(format!("failed to seek spill file: {error}")))?;

    let result = {
        let mut writer = BufWriter::new(&mut *file);
        let result = (|| {
            for batch in batches {
                serde_json::to_writer(&mut writer, &ExactBatch(batch)).map_err(|error| {
                    spill_error(format!("failed to serialize spill batch: {error}"))
                })?;
                writer.write_all(b"\n").map_err(|error| {
                    spill_error(format!("failed to write spill batch: {error}"))
                })?;
            }
            writer
                .flush()
                .map_err(|error| spill_error(format!("failed to flush spill file: {error}")))
        })();
        drop(writer);
        result
    };

    if let Err(error) = result {
        if let Err(rollback_error) = file.set_len(original_len) {
            return Err(spill_error(format!(
                "{error}; failed to roll back partial spill write: {rollback_error}"
            )));
        }
        file.seek(SeekFrom::End(0)).map_err(|rollback_error| {
            spill_error(format!(
                "{error}; failed to restore spill position: {rollback_error}"
            ))
        })?;
        return Err(error);
    }

    Ok(())
}

pub(super) struct ExactBatch<'a>(pub(super) &'a Batch);

impl Serialize for ExactBatch<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("schema", &self.0.schema.columns)?;
        map.serialize_entry("rows", &ExactRows(&self.0.rows))?;
        map.end()
    }
}

struct ExactRows<'a>(&'a [ResultRow]);

impl Serialize for ExactRows<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            sequence.serialize_element(&ExactRow(row))?;
        }
        sequence.end()
    }
}

pub(super) struct ExactRow<'a>(pub(super) &'a ResultRow);

impl Serialize for ExactRow<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, &ExactValue(value))?;
        }
        map.end()
    }
}

struct ExactValue<'a>(&'a Value);

impl Serialize for ExactValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Null => serializer.serialize_unit_variant("SpillValue", 0, "Null"),
            Value::Bool(value) => {
                serializer.serialize_newtype_variant("SpillValue", 1, "Bool", value)
            }
            Value::Int(value) => {
                serializer.serialize_newtype_variant("SpillValue", 2, "Int", value)
            }
            Value::Float(value) => {
                serializer.serialize_newtype_variant("SpillValue", 3, "Float", &value.to_bits())
            }
            Value::Str(value) => {
                serializer.serialize_newtype_variant("SpillValue", 4, "Str", value)
            }
            Value::Bytes(value) => {
                serializer.serialize_newtype_variant("SpillValue", 5, "Bytes", value)
            }
            Value::Temporal(value) => {
                serializer.serialize_newtype_variant("SpillValue", 6, "Temporal", value)
            }
            Value::Decimal(value) => serializer.serialize_newtype_variant(
                "SpillValue",
                7,
                "Decimal",
                &value.to_sql_string(),
            ),
            Value::List(value) => {
                serializer.serialize_newtype_variant("SpillValue", 8, "List", &ExactValues(value))
            }
            Value::Map(value) => {
                serializer.serialize_newtype_variant("SpillValue", 9, "Map", &ExactMap(value))
            }
        }
    }
}

struct ExactValues<'a>(&'a [Value]);

impl Serialize for ExactValues<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&ExactValue(value))?;
        }
        sequence.end()
    }
}

struct ExactMap<'a>(&'a BTreeMap<String, Value>);

impl Serialize for ExactMap<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &ExactValue(value))?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
struct StoredBatch {
    schema: Vec<String>,
    rows: Vec<BTreeMap<String, StoredValue>>,
}

pub(super) fn decode_row(record: &[u8]) -> ExecResult<ResultRow> {
    let stored: BTreeMap<String, StoredValue> =
        serde_json::from_slice(record).map_err(|error| {
            spill_error(format!("failed to deserialize indexed spill row: {error}"))
        })?;
    stored
        .into_iter()
        .map(|(name, value)| value.into_value().map(|value| (name, value)))
        .collect()
}

#[derive(Deserialize)]
enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(String),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl StoredValue {
    fn into_value(self) -> ExecResult<Value> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Int(value) => Ok(Value::Int(value)),
            Self::Float(bits) => Ok(Value::Float(f64::from_bits(bits))),
            Self::Str(value) => Ok(Value::Str(value)),
            Self::Bytes(value) => Ok(Value::Bytes(value)),
            Self::Temporal(value) => Ok(Value::Temporal(value)),
            Self::Decimal(value) => DecimalValue::parse(&value)
                .map(Value::Decimal)
                .ok_or_else(|| spill_error(format!("invalid decimal in spill file: {value}"))),
            Self::List(values) => values
                .into_iter()
                .map(Self::into_value)
                .collect::<ExecResult<Vec<_>>>()
                .map(Value::List),
            Self::Map(values) => values
                .into_iter()
                .map(|(key, value)| value.into_value().map(|value| (key, value)))
                .collect::<ExecResult<BTreeMap<_, _>>>()
                .map(Value::Map),
        }
    }
}

pub(super) fn decode_batch(record: &[u8]) -> ExecResult<Batch> {
    let stored: StoredBatch = serde_json::from_slice(record)
        .map_err(|error| spill_error(format!("failed to deserialize spill batch: {error}")))?;
    let rows = stored
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(name, value)| value.into_value().map(|value| (name, value)))
                .collect::<ExecResult<ResultRow>>()
        })
        .collect::<ExecResult<Vec<_>>>()?;
    Ok(Batch::new(RowSchema::new(stored.schema), rows))
}
