//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed codec for streaming INSERT preparation spill rows.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_execution::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use super::super::{
    decode_prepared_insert_conflict, encode_prepared_insert_conflict, PreparedInsertConflict,
};

const INSERT_SPILL_CODEC_VERSION: i64 = 1;
const INSERT_SPILL_WIDTH: usize = 1;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PreparedInsertSpillRow {
    pub(super) target_table: String,
    pub(super) document: Document,
    pub(super) conflict: PreparedInsertConflict,
}

pub(super) fn prepared_insert_spill_schema() -> RowSchema {
    RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None],
    )
}

pub(super) fn encode_prepared_insert_spill_row(prepared: PreparedInsertSpillRow) -> PhysicalRow {
    PhysicalRow::from_values(vec![Value::Map(BTreeMap::from([
        ("version".into(), Value::Int(INSERT_SPILL_CODEC_VERSION)),
        ("target_table".into(), Value::Str(prepared.target_table)),
        ("document".into(), Value::Map(prepared.document)),
        (
            "conflict".into(),
            encode_prepared_insert_conflict(prepared.conflict),
        ),
    ]))])
}

pub(super) fn decode_prepared_insert_spill_row(
    row: OwnedPhysicalRow,
) -> Result<PreparedInsertSpillRow, SQLError> {
    if row.schema.physical_width() != INSERT_SPILL_WIDTH {
        return Err(SQLError::Internal(format!(
            "INSERT prepared spill row has {} fields, expected {INSERT_SPILL_WIDTH}",
            row.schema.physical_width()
        )));
    }
    let Some(Value::Map(mut fields)) = row.physical_value_at(0).cloned() else {
        return Err(SQLError::Internal(
            "INSERT prepared spill row payload is not a map".into(),
        ));
    };
    match fields.remove("version") {
        Some(Value::Int(INSERT_SPILL_CODEC_VERSION)) => {}
        Some(Value::Int(version)) => {
            return Err(SQLError::Internal(format!(
                "INSERT prepared spill row has unsupported codec version {version}"
            )))
        }
        _ => {
            return Err(SQLError::Internal(
                "INSERT prepared spill row has no valid codec version".into(),
            ))
        }
    }
    let target_table = match fields.remove("target_table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "INSERT prepared spill row has no target table".into(),
            ))
        }
    };
    let document = match fields.remove("document") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "INSERT prepared spill row has no document".into(),
            ))
        }
    };
    let conflict =
        decode_prepared_insert_conflict(fields.remove("conflict").ok_or_else(|| {
            SQLError::Internal("INSERT prepared spill row has no conflict action".into())
        })?)?;
    if let Some(field) = fields.keys().next() {
        return Err(SQLError::Internal(format!(
            "INSERT prepared spill row has unknown field `{field}`"
        )));
    }
    Ok(PreparedInsertSpillRow {
        target_table,
        document,
        conflict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> PreparedInsertSpillRow {
        PreparedInsertSpillRow {
            target_table: "public.items".into(),
            document: BTreeMap::from([("id".into(), Value::Int(7))]),
            conflict: PreparedInsertConflict::Insert {
                doc_id: 7,
                supplied: true,
            },
        }
    }

    #[test]
    fn prepared_insert_spill_codec_round_trips() {
        let expected = prepared();
        let row = OwnedPhysicalRow::new(
            prepared_insert_spill_schema(),
            encode_prepared_insert_spill_row(expected.clone()),
        );
        assert_eq!(decode_prepared_insert_spill_row(row).unwrap(), expected);
    }

    #[test]
    fn prepared_insert_spill_codec_rejects_version_width_and_unknown_fields() {
        let wide = OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None, None],
            ),
            PhysicalRow::from_values(vec![Value::Null, Value::Null]),
        );
        assert!(decode_prepared_insert_spill_row(wide).is_err());

        let encoded = OwnedPhysicalRow::new(
            prepared_insert_spill_schema(),
            encode_prepared_insert_spill_row(prepared()),
        );
        let mut wrong_version = match encoded.physical_value_at(0) {
            Some(Value::Map(fields)) => fields.clone(),
            _ => unreachable!(),
        };
        wrong_version.insert("version".into(), Value::Int(INSERT_SPILL_CODEC_VERSION + 1));
        let wrong_version = OwnedPhysicalRow::new(
            prepared_insert_spill_schema(),
            PhysicalRow::from_values(vec![Value::Map(wrong_version)]),
        );
        assert!(decode_prepared_insert_spill_row(wrong_version).is_err());

        let encoded = OwnedPhysicalRow::new(
            prepared_insert_spill_schema(),
            encode_prepared_insert_spill_row(prepared()),
        );
        let mut unknown_field = match encoded.physical_value_at(0) {
            Some(Value::Map(fields)) => fields.clone(),
            _ => unreachable!(),
        };
        unknown_field.insert("unexpected".into(), Value::Null);
        let unknown_field = OwnedPhysicalRow::new(
            prepared_insert_spill_schema(),
            PhysicalRow::from_values(vec![Value::Map(unknown_field)]),
        );
        assert!(decode_prepared_insert_spill_row(unknown_field).is_err());
    }
}
