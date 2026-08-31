//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed codecs for MERGE pairing and prepared-action spill rows.

use uqa_core::{DocId, Value};
use uqa_execution::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_sql::{ast::ColumnType, SQLError};
use uqa_storage::document_store::Document;

use super::super::{
    decode_prepared_doc_id, decode_prepared_mutation_action, encode_prepared_doc_id,
    encode_prepared_mutation_action, PreparedMutationAction,
};

const MERGE_SPILL_CODEC_VERSION: i64 = 1;
const MERGE_PAIR_HEADER_WIDTH: usize = 5;
const MERGE_ACTION_WIDTH: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::sql) enum MergePairKind {
    Matched,
    NotMatchedBySource,
    NotMatchedByTarget,
}

impl MergePairKind {
    pub(in crate::sql) fn encode(self) -> i64 {
        match self {
            Self::Matched => 0,
            Self::NotMatchedBySource => 1,
            Self::NotMatchedByTarget => 2,
        }
    }

    pub(in crate::sql) fn decode(value: &Value) -> Result<Self, SQLError> {
        match value {
            Value::Int(0) => Ok(Self::Matched),
            Value::Int(1) => Ok(Self::NotMatchedBySource),
            Value::Int(2) => Ok(Self::NotMatchedByTarget),
            other => Err(SQLError::Internal(format!(
                "invalid spilled MERGE pairing kind {other:?}"
            ))),
        }
    }
}

pub(super) struct MergePairing {
    pub(super) kind: MergePairKind,
    pub(super) storage_table: Option<String>,
    pub(super) doc_id: Option<DocId>,
    pub(super) target_document: Option<Document>,
    pub(super) source_row: OwnedPhysicalRow,
}

pub(super) fn merge_pair_schema(source: &RowSchema) -> RowSchema {
    let header = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![
            Some(ColumnType::BigInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::Text),
            Some(ColumnType::Bytea),
            None,
        ],
    );
    RowSchema::join(&header, source, std::iter::empty())
}

pub(super) fn encode_merge_pair(
    kind: MergePairKind,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    target_document: Option<&Document>,
    source_row: &OwnedPhysicalRow,
) -> PhysicalRow {
    let header = PhysicalRow::from_values(vec![
        Value::Int(MERGE_SPILL_CODEC_VERSION),
        Value::Int(kind.encode()),
        storage_table.map_or(Value::Null, |table| Value::Str(table.to_string())),
        doc_id.map_or(Value::Null, encode_prepared_doc_id),
        target_document.map_or(Value::Null, |document| Value::Map(document.clone())),
    ]);
    PhysicalRow::concat(&header, &source_row.row)
}

pub(super) fn decode_merge_pair(encoded: OwnedPhysicalRow) -> Result<MergePairing, SQLError> {
    if encoded.schema.physical_width() < MERGE_PAIR_HEADER_WIDTH {
        return Err(SQLError::Internal(format!(
            "spilled MERGE pairing has {} fields, expected at least {MERGE_PAIR_HEADER_WIDTH}",
            encoded.schema.physical_width()
        )));
    }
    match encoded.physical_value_at(0) {
        Some(Value::Int(MERGE_SPILL_CODEC_VERSION)) => {}
        Some(Value::Int(version)) => {
            return Err(SQLError::Internal(format!(
                "spilled MERGE pairing has unsupported codec version {version}"
            )))
        }
        _ => {
            return Err(SQLError::Internal(
                "spilled MERGE pairing has no valid codec version".into(),
            ))
        }
    }
    let kind =
        MergePairKind::decode(encoded.physical_value_at(1).ok_or_else(|| {
            SQLError::Internal("spilled MERGE pairing lost its match kind".into())
        })?)?;
    let storage_table = match encoded.physical_value_at(2) {
        Some(Value::Str(table)) => Some(table.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE storage table value {value:?}"
            )))
        }
    };
    let doc_id = match encoded.physical_value_at(3) {
        Some(Value::Null) | None => None,
        Some(value) => Some(decode_prepared_doc_id(
            value.clone(),
            "spilled MERGE pairing",
        )?),
    };
    let target_document = match encoded.physical_value_at(4) {
        Some(Value::Map(document)) => Some(document.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE target document value {value:?}"
            )))
        }
    };
    match kind {
        MergePairKind::Matched | MergePairKind::NotMatchedBySource
            if storage_table.is_none() || doc_id.is_none() || target_document.is_none() =>
        {
            return Err(SQLError::Internal(
                "target-bearing MERGE pairing lost its target row".into(),
            ));
        }
        MergePairKind::NotMatchedByTarget
            if storage_table.is_some() || doc_id.is_some() || target_document.is_some() =>
        {
            return Err(SQLError::Internal(
                "target-missing MERGE pairing unexpectedly retained a target row".into(),
            ));
        }
        _ => {}
    }
    Ok(MergePairing {
        kind,
        storage_table,
        doc_id,
        target_document,
        source_row: encoded,
    })
}

pub(in crate::sql) fn merge_source_index_value(index: usize) -> Value {
    Value::Str(index.to_string())
}

pub(super) fn prepared_mutation_action_schema() -> RowSchema {
    RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None],
    )
}

pub(super) fn push_prepared_mutation_action(
    buffer: &mut uqa_execution::SpillBuffer,
    schema: &RowSchema,
    action: PreparedMutationAction,
) -> Result<(), SQLError> {
    buffer
        .push(uqa_execution::Batch::from_physical_rows(
            schema.clone(),
            vec![PhysicalRow::from_values(vec![
                encode_prepared_mutation_action(action),
            ])],
        ))
        .map_err(crate::sql::select::physical_exec_error)?;
    Ok(())
}

pub(super) fn decode_prepared_mutation_action_row(
    row: OwnedPhysicalRow,
) -> Result<PreparedMutationAction, SQLError> {
    if row.schema.physical_width() != MERGE_ACTION_WIDTH {
        return Err(SQLError::Internal(format!(
            "MERGE prepared action spill has {} fields, expected {MERGE_ACTION_WIDTH}",
            row.schema.physical_width()
        )));
    }
    let payload = row
        .physical_value_at(0)
        .cloned()
        .ok_or_else(|| SQLError::Internal("MERGE prepared action spill lost its payload".into()))?;
    decode_prepared_mutation_action(payload)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::super::{PreparedDocumentInsert, PreparedMutationAction};
    use super::*;

    fn source_row() -> OwnedPhysicalRow {
        OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![Some(ColumnType::BigInteger)],
            ),
            PhysicalRow::from_values(vec![Value::Int(17)]),
        )
    }

    fn target_document() -> Document {
        BTreeMap::from([("id".into(), Value::Int(7))])
    }

    #[test]
    fn merge_pair_codec_round_trips_each_match_kind() {
        for (kind, table, doc_id, document) in [
            (
                MergePairKind::Matched,
                Some("public.items"),
                Some(7),
                Some(target_document()),
            ),
            (
                MergePairKind::NotMatchedBySource,
                Some("public.items"),
                Some(7),
                Some(target_document()),
            ),
            (MergePairKind::NotMatchedByTarget, None, None, None),
        ] {
            let source = source_row();
            let schema = merge_pair_schema(&source.schema);
            let encoded = OwnedPhysicalRow::new(
                schema,
                encode_merge_pair(kind, table, doc_id, document.as_ref(), &source),
            );
            let decoded = decode_merge_pair(encoded).unwrap();
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.storage_table.as_deref(), table);
            assert_eq!(decoded.doc_id, doc_id);
            assert_eq!(decoded.target_document, document);
        }
    }

    #[test]
    fn merge_pair_codec_rejects_version_width_and_identity_errors() {
        let narrow = OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None; MERGE_PAIR_HEADER_WIDTH - 1],
            ),
            PhysicalRow::nulls(MERGE_PAIR_HEADER_WIDTH - 1),
        );
        assert!(decode_merge_pair(narrow).is_err());

        let source = source_row();
        let schema = merge_pair_schema(&source.schema);
        let wrong_version = PhysicalRow::concat(
            &PhysicalRow::from_values(vec![
                Value::Int(MERGE_SPILL_CODEC_VERSION + 1),
                Value::Int(MergePairKind::Matched.encode()),
                Value::Str("public.items".into()),
                encode_prepared_doc_id(7),
                Value::Map(target_document()),
            ]),
            &source.row,
        );
        assert!(decode_merge_pair(OwnedPhysicalRow::new(schema.clone(), wrong_version)).is_err());

        let invalid_identity = PhysicalRow::concat(
            &PhysicalRow::from_values(vec![
                Value::Int(MERGE_SPILL_CODEC_VERSION),
                Value::Int(MergePairKind::Matched.encode()),
                Value::Str("public.items".into()),
                Value::Bytes(vec![0; 3]),
                Value::Map(target_document()),
            ]),
            &source.row,
        );
        assert!(decode_merge_pair(OwnedPhysicalRow::new(schema, invalid_identity)).is_err());
    }

    #[test]
    fn merge_prepared_action_row_codec_round_trips_and_rejects_width() {
        let expected = PreparedMutationAction::Insert(PreparedDocumentInsert {
            table: "public.items".into(),
            doc_id: 11,
            document: target_document(),
        });
        let row = OwnedPhysicalRow::new(
            prepared_mutation_action_schema(),
            PhysicalRow::from_values(vec![encode_prepared_mutation_action(expected.clone())]),
        );
        assert_eq!(decode_prepared_mutation_action_row(row).unwrap(), expected);

        let wide = OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None, None],
            ),
            PhysicalRow::from_values(vec![Value::Null, Value::Null]),
        );
        assert!(decode_prepared_mutation_action_row(wide).is_err());
    }
}
