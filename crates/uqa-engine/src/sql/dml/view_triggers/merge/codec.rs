//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed codec for trigger-backed view MERGE pairing spill rows.

use uqa_core::Value;
use uqa_execution::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_sql::SQLError;

use super::super::super::MergePairKind;

const VIEW_MERGE_SPILL_CODEC_VERSION: i64 = 1;
const VIEW_MERGE_PAIR_HEADER_WIDTH: usize = 3;

pub(super) struct ViewMergePair {
    pub(super) kind: MergePairKind,
    pub(super) target: Option<Vec<Value>>,
    pub(super) source: OwnedPhysicalRow,
}

pub(super) fn view_merge_pair_schema(source: &RowSchema) -> RowSchema {
    let header = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![
            Some(uqa_sql::ast::ColumnType::BigInteger),
            Some(uqa_sql::ast::ColumnType::BigInteger),
            None,
        ],
    );
    RowSchema::join(&header, source, std::iter::empty())
}

fn encode_view_merge_pair(
    kind: MergePairKind,
    target: Option<&[Value]>,
    source: &OwnedPhysicalRow,
) -> PhysicalRow {
    let header = PhysicalRow::from_values(vec![
        Value::Int(VIEW_MERGE_SPILL_CODEC_VERSION),
        Value::Int(kind.encode()),
        target.map_or(Value::Null, |values| Value::List(values.to_vec())),
    ]);
    PhysicalRow::concat(&header, &source.row)
}

pub(super) fn decode_view_merge_pair(encoded: OwnedPhysicalRow) -> Result<ViewMergePair, SQLError> {
    if encoded.schema.physical_width() < VIEW_MERGE_PAIR_HEADER_WIDTH {
        return Err(SQLError::Internal(format!(
            "view MERGE pair has {} fields, expected at least {VIEW_MERGE_PAIR_HEADER_WIDTH}",
            encoded.schema.physical_width()
        )));
    }
    match encoded.physical_value_at(0) {
        Some(Value::Int(VIEW_MERGE_SPILL_CODEC_VERSION)) => {}
        Some(Value::Int(version)) => {
            return Err(SQLError::Internal(format!(
                "view MERGE pair has unsupported codec version {version}"
            )))
        }
        _ => {
            return Err(SQLError::Internal(
                "view MERGE pair has no valid codec version".into(),
            ))
        }
    }
    let kind = MergePairKind::decode(
        encoded
            .physical_value_at(1)
            .ok_or_else(|| SQLError::Internal("view MERGE pair lost its kind".into()))?,
    )?;
    let target = match encoded.physical_value_at(2) {
        Some(Value::List(values)) => Some(values.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "view MERGE pair has invalid target row {value:?}"
            )))
        }
    };
    if matches!(
        kind,
        MergePairKind::Matched | MergePairKind::NotMatchedBySource
    ) != target.is_some()
    {
        return Err(SQLError::Internal(
            "view MERGE pair target presence does not match its kind".into(),
        ));
    }
    Ok(ViewMergePair {
        kind,
        target,
        source: encoded,
    })
}

pub(super) fn push_view_merge_pair(
    pairings: &mut uqa_execution::SpillBuffer,
    schema: &RowSchema,
    kind: MergePairKind,
    target: Option<&[Value]>,
    source: &OwnedPhysicalRow,
) -> Result<(), SQLError> {
    pairings
        .push(uqa_execution::Batch::from_physical_rows(
            schema.clone(),
            vec![encode_view_merge_pair(kind, target, source)],
        ))
        .map(|_| ())
        .map_err(crate::sql::select::physical_exec_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_row() -> OwnedPhysicalRow {
        OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![Some(uqa_sql::ast::ColumnType::Text)],
            ),
            PhysicalRow::from_values(vec![Value::Str("source".into())]),
        )
    }

    #[test]
    fn view_merge_pair_codec_round_trips_each_match_kind() {
        for (kind, target) in [
            (MergePairKind::Matched, Some(vec![Value::Int(1)])),
            (MergePairKind::NotMatchedBySource, Some(vec![Value::Int(1)])),
            (MergePairKind::NotMatchedByTarget, None),
        ] {
            let source = source_row();
            let encoded = OwnedPhysicalRow::new(
                view_merge_pair_schema(&source.schema),
                encode_view_merge_pair(kind, target.as_deref(), &source),
            );
            let decoded = decode_view_merge_pair(encoded).unwrap();
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.target, target);
        }
    }

    #[test]
    fn view_merge_pair_codec_rejects_version_width_and_shape_errors() {
        let narrow = OwnedPhysicalRow::new(
            RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None; VIEW_MERGE_PAIR_HEADER_WIDTH - 1],
            ),
            PhysicalRow::nulls(VIEW_MERGE_PAIR_HEADER_WIDTH - 1),
        );
        assert!(decode_view_merge_pair(narrow).is_err());

        let source = source_row();
        let schema = view_merge_pair_schema(&source.schema);
        let wrong_version = PhysicalRow::concat(
            &PhysicalRow::from_values(vec![
                Value::Int(VIEW_MERGE_SPILL_CODEC_VERSION + 1),
                Value::Int(MergePairKind::Matched.encode()),
                Value::List(vec![Value::Int(1)]),
            ]),
            &source.row,
        );
        assert!(
            decode_view_merge_pair(OwnedPhysicalRow::new(schema.clone(), wrong_version)).is_err()
        );

        let missing_target = PhysicalRow::concat(
            &PhysicalRow::from_values(vec![
                Value::Int(VIEW_MERGE_SPILL_CODEC_VERSION),
                Value::Int(MergePairKind::Matched.encode()),
                Value::Null,
            ]),
            &source.row,
        );
        assert!(decode_view_merge_pair(OwnedPhysicalRow::new(schema, missing_target)).is_err());
    }
}
