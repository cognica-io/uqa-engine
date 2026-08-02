//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary value codecs and ordered key layout shared by key/value adapters.

use super::{
    BTreeMap, Deserialize, DocId, Document, Serialize, StorageBackendError, StorageBackendResult,
    Value, DOCUMENT_VALUE_V1_PREFIX, TAG_DOCUMENT, TAG_DOC_LENGTH, TAG_FIELD_STATS, TAG_POSTING,
    TAG_REVERSE_POSTING, TAG_VECTOR,
};

pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    for index in (0..upper.len()).rev() {
        if upper[index] != u8::MAX {
            upper[index] += 1;
            upper.truncate(index + 1);
            return Some(upper);
        }
    }
    None
}

pub(super) fn other_error(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

pub(super) fn encode_value<T: Serialize>(value: &T) -> StorageBackendResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(StorageBackendError::from)
}

pub(super) fn decode_value<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> StorageBackendResult<T> {
    serde_json::from_slice(bytes).map_err(StorageBackendError::from)
}

pub(super) fn encode_document_value(document: &Document) -> StorageBackendResult<Vec<u8>> {
    let body = encode_value(document)?;
    let capacity = DOCUMENT_VALUE_V1_PREFIX
        .len()
        .checked_add(body.len())
        .ok_or_else(|| other_error("KeyValue document encoding size overflow"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|error| other_error(format!("cannot allocate KeyValue document: {error}")))?;
    encoded.extend_from_slice(DOCUMENT_VALUE_V1_PREFIX);
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

pub(super) fn decode_document_value(bytes: &[u8]) -> StorageBackendResult<Document> {
    if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V1_PREFIX) {
        return decode_value(body);
    }
    decode_legacy_document_value(bytes)
}

/// Decode the unversioned representation written before `Value::Bytes` gained
/// an explicit JSON tag. The historical decoder selected bytes for every
/// empty or byte-range integer array, including nested arrays.
pub(super) fn decode_legacy_document_value(bytes: &[u8]) -> StorageBackendResult<Document> {
    let serde_json::Value::Object(fields) = serde_json::from_slice(bytes)? else {
        return Err(other_error(
            "persisted KeyValue document is not a JSON object",
        ));
    };
    fields
        .into_iter()
        .map(|(field, value)| Ok((field, decode_legacy_json_value(value)?)))
        .collect()
}

pub(super) fn decode_legacy_json_value(value: serde_json::Value) -> StorageBackendResult<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_u64() {
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(other_error(
                    "persisted KeyValue document number is outside the supported numeric range",
                ))
            }
        }
        serde_json::Value::String(value) => Ok(Value::Str(value)),
        serde_json::Value::Array(values) => {
            let mut decoded = Vec::new();
            decoded.try_reserve_exact(values.len()).map_err(|error| {
                other_error(format!(
                    "cannot allocate legacy KeyValue document array: {error}"
                ))
            })?;
            for value in values {
                decoded.push(decode_legacy_json_value(value)?);
            }

            let mut bytes = Vec::new();
            bytes.try_reserve_exact(decoded.len()).map_err(|error| {
                other_error(format!(
                    "cannot allocate legacy KeyValue document byte array: {error}"
                ))
            })?;
            for value in &decoded {
                let Value::Int(value) = value else {
                    return Ok(Value::List(decoded));
                };
                let Ok(value) = u8::try_from(*value) else {
                    return Ok(Value::List(decoded));
                };
                bytes.push(value);
            }
            Ok(Value::Bytes(bytes))
        }
        serde_json::Value::Object(values) => {
            if values.contains_key("$uqa_type") {
                let tagged = serde_json::Value::Object(values.clone());
                let decoded = serde_json::from_value::<Value>(tagged)?;
                if !matches!(decoded, Value::Map(_)) {
                    return Ok(decoded);
                }
            }
            let mut decoded = BTreeMap::new();
            for (key, value) in values {
                decoded.insert(key, decode_legacy_json_value(value)?);
            }
            Ok(Value::Map(decoded))
        }
    }
}

pub(super) fn string_value(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

pub(super) fn decode_string(bytes: Vec<u8>) -> StorageBackendResult<String> {
    String::from_utf8(bytes).map_err(|err| other_error(format!("invalid utf-8 value: {err}")))
}

pub(super) fn key_with_tag(tag: u8) -> Vec<u8> {
    vec![tag]
}

pub(super) fn key_segment_length(len: usize) -> StorageBackendResult<u32> {
    len.try_into()
        .map_err(|_| other_error("KeyValue key segment exceeds the u32 on-disk format"))
}

pub(super) fn push_segment(key: &mut Vec<u8>, segment: &[u8]) -> StorageBackendResult<()> {
    let len = key_segment_length(segment.len())?;
    key.extend_from_slice(&len.to_be_bytes());
    key.extend_from_slice(segment);
    Ok(())
}

pub(super) fn push_str(key: &mut Vec<u8>, segment: &str) -> StorageBackendResult<()> {
    push_segment(key, segment.as_bytes())
}

pub(super) fn push_u64(key: &mut Vec<u8>, value: u64) {
    key.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn read_segment<'a>(
    key: &'a [u8],
    offset: &mut usize,
) -> StorageBackendResult<&'a [u8]> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| other_error("KeyValue key offset overflow"))?;
    let len_bytes: [u8; 4] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue key segment length"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue key segment length"))?;
    *offset = end;
    let len = usize::try_from(u32::from_be_bytes(len_bytes))
        .map_err(|_| other_error("KeyValue key segment length exceeds usize"))?;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| other_error("KeyValue key segment end overflow"))?;
    let segment = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue key segment"))?;
    *offset = end;
    Ok(segment)
}

pub(super) fn read_str(key: &[u8], offset: &mut usize) -> StorageBackendResult<String> {
    let segment = read_segment(key, offset)?;
    std::str::from_utf8(segment)
        .map(str::to_string)
        .map_err(|err| other_error(format!("invalid utf-8 key segment: {err}")))
}

pub(super) fn read_u64(key: &[u8], offset: &mut usize) -> StorageBackendResult<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| other_error("KeyValue key offset overflow"))?;
    let bytes: [u8; 8] = key
        .get(*offset..end)
        .ok_or_else(|| other_error("truncated KeyValue u64 key segment"))?
        .try_into()
        .map_err(|_| other_error("invalid KeyValue u64 key segment"))?;
    *offset = end;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn table_prefixed_key(tag: u8, table: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, table)?;
    Ok(key)
}

pub(super) fn single_str_key(tag: u8, name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, name)?;
    Ok(key)
}

pub(super) fn u64_value(value: u64) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

pub(super) fn decode_u64_value(bytes: &[u8]) -> StorageBackendResult<u64> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| other_error("invalid u64 KeyValue payload"))?;
    Ok(u64::from_be_bytes(array))
}

pub(super) fn positions_to_blob(positions: &[u32]) -> StorageBackendResult<Vec<u8>> {
    let capacity = positions
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| other_error("posting-position payload size overflow"))?;
    let mut buf = Vec::with_capacity(capacity);
    for p in positions {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    Ok(buf)
}

pub(super) fn blob_to_positions(blob: &[u8]) -> StorageBackendResult<Vec<u32>> {
    if blob.len() % 4 != 0 {
        return Err(other_error("invalid posting positions KeyValue payload"));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

pub(super) fn vector_to_blob(v: &[f32]) -> StorageBackendResult<Vec<u8>> {
    let capacity = v
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| other_error("vector payload size overflow"))?;
    let mut buf = Vec::with_capacity(capacity);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    Ok(buf)
}

pub(super) fn usize_to_u64(value: usize, context: &str) -> StorageBackendResult<u64> {
    u64::try_from(value).map_err(|_| other_error(format!("{context} exceeds u64")))
}

pub(super) fn validate_vector_ordinal_count(count: u64) -> StorageBackendResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(other_error("vector ordinal exceeds u32 index format"));
    }
    Ok(())
}

pub(super) fn blob_to_vector(blob: &[u8]) -> StorageBackendResult<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(other_error("invalid vector KeyValue payload"));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

pub(super) fn document_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_DOCUMENT, table)
}

pub(super) fn document_key(table: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = document_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn posting_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_POSTING, table)
}

pub(super) fn posting_field_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn posting_term_prefix(
    table: &str,
    field: &str,
    term: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_field_prefix(table, field)?;
    push_str(&mut key, term)?;
    Ok(key)
}

pub(super) fn posting_key(
    table: &str,
    field: &str,
    term: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = posting_term_prefix(table, field, term)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn doc_length_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_DOC_LENGTH, table)
}

pub(super) fn doc_length_doc_prefix(table: &str, doc_id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = doc_length_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn doc_length_key(
    table: &str,
    doc_id: DocId,
    field: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = doc_length_doc_prefix(table, doc_id)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn field_stats_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_FIELD_STATS, table)
}

pub(super) fn field_stats_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = field_stats_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn reverse_posting_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_REVERSE_POSTING, table)
}

pub(super) fn reverse_posting_doc_prefix(
    table: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = reverse_posting_key_prefix(table)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn reverse_posting_key(
    table: &str,
    doc_id: DocId,
    field: &str,
    term: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = reverse_posting_doc_prefix(table, doc_id)?;
    push_str(&mut key, field)?;
    push_str(&mut key, term)?;
    Ok(key)
}

pub(super) fn vector_key_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    table_prefixed_key(TAG_VECTOR, table)
}

pub(super) fn vector_field_prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_key_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn vector_doc_prefix(
    table: &str,
    field: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_field_prefix(table, field)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn vector_key(
    table: &str,
    field: &str,
    doc_id: DocId,
    vector_ordinal: u32,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = vector_doc_prefix(table, field, doc_id)?;
    push_u64(&mut key, u64::from(vector_ordinal));
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::prefix_upper_bound;

    #[test]
    fn prefix_upper_bound_carries_and_truncates_trailing_max_bytes() {
        assert_eq!(prefix_upper_bound(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(prefix_upper_bound(&[b'a', 0xff]), Some(vec![b'b']));
        assert_eq!(prefix_upper_bound(&[0xff, 0xff]), None);
        assert_eq!(prefix_upper_bound(&[]), None);
    }
}
