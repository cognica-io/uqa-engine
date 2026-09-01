//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed prepared rewrite/delete trees and their private spill codec.

use std::collections::BTreeMap;

use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

const PREPARED_MUTATION_CODEC_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) struct PreparedDocumentRewrite {
    pub table: String,
    pub doc_id: DocId,
    pub destination: Option<(String, DocId)>,
    pub partition_move_delete: Option<Box<PreparedDocumentDelete>>,
    pub old_document: Document,
    pub new_document: Document,
    pub actions: Vec<PreparedDocumentRewrite>,
    pub trigger_updated_columns: Option<Vec<String>>,
    pub capture_partition_move_update_transition: bool,
}

impl PreparedDocumentRewrite {
    pub(in crate::sql) fn is_partition_move_delete(&self) -> bool {
        self.partition_move_delete.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) enum PreparedDeleteAction {
    Delete(Box<PreparedDocumentDelete>),
    Rewrite(Box<PreparedDocumentRewrite>),
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) struct PreparedDocumentDelete {
    pub table: String,
    pub doc_id: DocId,
    pub document: Document,
    pub actions: Vec<PreparedDeleteAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) struct PreparedDocumentInsert {
    pub table: String,
    pub doc_id: DocId,
    pub document: Document,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) enum PreparedMutationAction {
    Insert(PreparedDocumentInsert),
    Rewrite(PreparedDocumentRewrite),
    Delete(PreparedDocumentDelete),
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) enum PreparedInsertConflict {
    Unresolved,
    Insert { doc_id: DocId, supplied: bool },
    Skip,
    Updated(PreparedDocumentRewrite),
}

pub(in crate::sql) fn encode_prepared_doc_id(doc_id: DocId) -> Value {
    Value::Bytes(doc_id.to_be_bytes().to_vec())
}

pub(in crate::sql) fn decode_prepared_doc_id(
    value: Value,
    context: &str,
) -> Result<DocId, SQLError> {
    let Value::Bytes(bytes) = value else {
        return Err(SQLError::Internal(format!(
            "{context} has a non-binary document id"
        )));
    };
    let bytes: [u8; std::mem::size_of::<DocId>()] = bytes
        .try_into()
        .map_err(|_| SQLError::Internal(format!("{context} has an invalid document id width")))?;
    Ok(DocId::from_be_bytes(bytes))
}

fn take_codec_version(fields: &mut BTreeMap<String, Value>, context: &str) -> Result<(), SQLError> {
    match fields.remove("version") {
        Some(Value::Int(PREPARED_MUTATION_CODEC_VERSION)) => Ok(()),
        Some(Value::Int(version)) => Err(SQLError::Internal(format!(
            "{context} has unsupported codec version {version}"
        ))),
        _ => Err(SQLError::Internal(format!(
            "{context} has no valid codec version"
        ))),
    }
}

fn reject_unknown_fields(fields: &BTreeMap<String, Value>, context: &str) -> Result<(), SQLError> {
    if fields.is_empty() {
        return Ok(());
    }
    Err(SQLError::Internal(format!(
        "{context} has unknown field `{}`",
        fields.keys().next().expect("non-empty map")
    )))
}

pub(in crate::sql) fn encode_prepared_document_rewrite(prepared: PreparedDocumentRewrite) -> Value {
    Value::Map(BTreeMap::from([
        (
            "version".into(),
            Value::Int(PREPARED_MUTATION_CODEC_VERSION),
        ),
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        (
            "destination".into(),
            prepared.destination.map_or(Value::Null, |(table, doc_id)| {
                Value::Map(BTreeMap::from([
                    ("table".into(), Value::Str(table)),
                    ("doc_id".into(), encode_prepared_doc_id(doc_id)),
                ]))
            }),
        ),
        (
            "partition_move_delete".into(),
            prepared
                .partition_move_delete
                .map_or(Value::Null, |delete| {
                    encode_prepared_document_delete(*delete)
                }),
        ),
        ("old".into(), Value::Map(prepared.old_document)),
        ("new".into(), Value::Map(prepared.new_document)),
        (
            "actions".into(),
            Value::List(
                prepared
                    .actions
                    .into_iter()
                    .map(encode_prepared_document_rewrite)
                    .collect(),
            ),
        ),
        (
            "trigger_updated_columns".into(),
            prepared
                .trigger_updated_columns
                .map_or(Value::Null, |columns| {
                    Value::List(columns.into_iter().map(Value::Str).collect())
                }),
        ),
        (
            "capture_partition_move_update_transition".into(),
            Value::Bool(prepared.capture_partition_move_update_transition),
        ),
    ]))
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(in crate::sql) fn decode_prepared_document_rewrite(
    value: Value,
) -> Result<PreparedDocumentRewrite, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared rewrite spill payload is not a map".into(),
        ));
    };
    take_codec_version(&mut fields, "prepared rewrite spill payload")?;
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared rewrite spill payload has no document id".into())
        })?,
        "prepared rewrite spill payload",
    )?;
    let destination = match fields.remove("destination") {
        Some(Value::Null) | None => None,
        Some(Value::Map(mut destination)) => {
            let table = match destination.remove("table") {
                Some(Value::Str(table)) => table,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared rewrite destination has no table".into(),
                    ))
                }
            };
            let doc_id = decode_prepared_doc_id(
                destination.remove("doc_id").ok_or_else(|| {
                    SQLError::Internal("prepared rewrite destination has no document id".into())
                })?,
                "prepared rewrite destination",
            )?;
            reject_unknown_fields(&destination, "prepared rewrite destination")?;
            Some((table, doc_id))
        }
        Some(_) => {
            return Err(SQLError::Internal(
                "prepared rewrite destination is not a map".into(),
            ))
        }
    };
    let partition_move_delete = match fields.remove("partition_move_delete") {
        Some(Value::Null) | None => None,
        Some(delete) => Some(Box::new(decode_prepared_document_delete(delete)?)),
    };
    let old_document = match fields.remove("old") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no old document".into(),
            ))
        }
    };
    let new_document = match fields.remove("new") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no new document".into(),
            ))
        }
    };
    let actions = match fields.remove("actions") {
        Some(Value::List(actions)) => actions
            .into_iter()
            .map(decode_prepared_document_rewrite)
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no action list".into(),
            ))
        }
    };
    let trigger_updated_columns = match fields.remove("trigger_updated_columns") {
        Some(Value::Null) => None,
        Some(Value::List(columns)) => Some(
            columns
                .into_iter()
                .map(|column| match column {
                    Value::Str(column) => Ok(column),
                    _ => Err(SQLError::Internal(
                        "prepared rewrite spill payload has a non-text trigger column".into(),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no trigger column list".into(),
            ))
        }
    };
    let capture_partition_move_update_transition = match fields
        .remove("capture_partition_move_update_transition")
    {
        Some(Value::Bool(capture)) => capture,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no partition movement transition mode".into(),
            ))
        }
    };
    reject_unknown_fields(&fields, "prepared rewrite spill payload")?;
    Ok(PreparedDocumentRewrite {
        table,
        doc_id,
        destination,
        partition_move_delete,
        old_document,
        new_document,
        actions,
        trigger_updated_columns,
        capture_partition_move_update_transition,
    })
}

pub(in crate::sql) fn encode_prepared_document_delete(prepared: PreparedDocumentDelete) -> Value {
    let actions = prepared
        .actions
        .into_iter()
        .map(|action| match action {
            PreparedDeleteAction::Delete(delete) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("delete".into())),
                ("plan".into(), encode_prepared_document_delete(*delete)),
            ])),
            PreparedDeleteAction::Rewrite(rewrite) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("rewrite".into())),
                ("plan".into(), encode_prepared_document_rewrite(*rewrite)),
            ])),
        })
        .collect();
    Value::Map(BTreeMap::from([
        (
            "version".into(),
            Value::Int(PREPARED_MUTATION_CODEC_VERSION),
        ),
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        ("document".into(), Value::Map(prepared.document)),
        ("actions".into(), Value::List(actions)),
    ]))
}

pub(in crate::sql) fn decode_prepared_document_delete(
    value: Value,
) -> Result<PreparedDocumentDelete, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared delete spill payload is not a map".into(),
        ));
    };
    take_codec_version(&mut fields, "prepared delete spill payload")?;
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared delete spill payload has no document id".into())
        })?,
        "prepared delete spill payload",
    )?;
    let document = match fields.remove("document") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no document".into(),
            ))
        }
    };
    let action_values = match fields.remove("actions") {
        Some(Value::List(actions)) => actions,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no action list".into(),
            ))
        }
    };
    reject_unknown_fields(&fields, "prepared delete spill payload")?;
    let mut actions = Vec::with_capacity(action_values.len());
    for action in action_values {
        let Value::Map(mut action) = action else {
            return Err(SQLError::Internal(
                "prepared delete action spill payload is not a map".into(),
            ));
        };
        let kind = match action.remove("kind") {
            Some(Value::Str(kind)) => kind,
            _ => {
                return Err(SQLError::Internal(
                    "prepared delete action spill payload has no kind".into(),
                ))
            }
        };
        let plan = action.remove("plan").ok_or_else(|| {
            SQLError::Internal("prepared delete action spill payload has no plan".into())
        })?;
        reject_unknown_fields(&action, "prepared delete action spill payload")?;
        actions.push(match kind.as_str() {
            "delete" => {
                PreparedDeleteAction::Delete(Box::new(decode_prepared_document_delete(plan)?))
            }
            "rewrite" => {
                PreparedDeleteAction::Rewrite(Box::new(decode_prepared_document_rewrite(plan)?))
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "prepared delete action spill payload has unknown kind `{kind}`"
                )))
            }
        });
    }
    Ok(PreparedDocumentDelete {
        table,
        doc_id,
        document,
        actions,
    })
}

pub(in crate::sql) fn encode_prepared_insert_conflict(prepared: PreparedInsertConflict) -> Value {
    let mut fields = BTreeMap::from([(
        "version".into(),
        Value::Int(PREPARED_MUTATION_CODEC_VERSION),
    )]);
    match prepared {
        PreparedInsertConflict::Unresolved => {
            fields.insert("kind".into(), Value::Str("unresolved".into()));
        }
        PreparedInsertConflict::Insert { doc_id, supplied } => {
            fields.insert("kind".into(), Value::Str("insert".into()));
            fields.insert("doc_id".into(), encode_prepared_doc_id(doc_id));
            fields.insert("supplied".into(), Value::Bool(supplied));
        }
        PreparedInsertConflict::Skip => {
            fields.insert("kind".into(), Value::Str("skip".into()));
        }
        PreparedInsertConflict::Updated(rewrite) => {
            fields.insert("kind".into(), Value::Str("updated".into()));
            fields.insert("rewrite".into(), encode_prepared_document_rewrite(rewrite));
        }
    }
    Value::Map(fields)
}

pub(in crate::sql) fn decode_prepared_insert_conflict(
    value: Value,
) -> Result<PreparedInsertConflict, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared INSERT conflict payload is not a map".into(),
        ));
    };
    take_codec_version(&mut fields, "prepared INSERT conflict payload")?;
    let kind = match fields.remove("kind") {
        Some(Value::Str(kind)) => kind,
        _ => {
            return Err(SQLError::Internal(
                "prepared INSERT conflict payload has no kind".into(),
            ))
        }
    };
    let prepared = match kind.as_str() {
        "unresolved" => PreparedInsertConflict::Unresolved,
        "insert" => PreparedInsertConflict::Insert {
            doc_id: decode_prepared_doc_id(
                fields.remove("doc_id").ok_or_else(|| {
                    SQLError::Internal("prepared INSERT payload has no document identity".into())
                })?,
                "prepared INSERT action",
            )?,
            supplied: match fields.remove("supplied") {
                Some(Value::Bool(supplied)) => supplied,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared INSERT payload has no supplied-id flag".into(),
                    ))
                }
            },
        },
        "skip" => PreparedInsertConflict::Skip,
        "updated" => PreparedInsertConflict::Updated(decode_prepared_document_rewrite(
            fields.remove("rewrite").ok_or_else(|| {
                SQLError::Internal("prepared INSERT conflict payload has no rewrite plan".into())
            })?,
        )?),
        _ => {
            return Err(SQLError::Internal(format!(
                "prepared INSERT conflict payload has unknown kind `{kind}`"
            )))
        }
    };
    reject_unknown_fields(&fields, "prepared INSERT conflict payload")?;
    Ok(prepared)
}

pub(in crate::sql) fn encode_prepared_mutation_action(action: PreparedMutationAction) -> Value {
    let (kind, plan) = match action {
        PreparedMutationAction::Insert(insert) => (
            "insert",
            Value::Map(BTreeMap::from([
                ("table".into(), Value::Str(insert.table)),
                ("doc_id".into(), encode_prepared_doc_id(insert.doc_id)),
                ("document".into(), Value::Map(insert.document)),
            ])),
        ),
        PreparedMutationAction::Rewrite(rewrite) => {
            ("rewrite", encode_prepared_document_rewrite(rewrite))
        }
        PreparedMutationAction::Delete(delete) => {
            ("delete", encode_prepared_document_delete(delete))
        }
    };
    Value::Map(BTreeMap::from([
        (
            "version".into(),
            Value::Int(PREPARED_MUTATION_CODEC_VERSION),
        ),
        ("kind".into(), Value::Str(kind.into())),
        ("plan".into(), plan),
    ]))
}

pub(in crate::sql) fn decode_prepared_mutation_action(
    value: Value,
) -> Result<PreparedMutationAction, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared mutation action payload is not a map".into(),
        ));
    };
    take_codec_version(&mut fields, "prepared mutation action payload")?;
    let kind = match fields.remove("kind") {
        Some(Value::Str(kind)) => kind,
        _ => {
            return Err(SQLError::Internal(
                "prepared mutation action payload has no kind".into(),
            ))
        }
    };
    let plan = fields
        .remove("plan")
        .ok_or_else(|| SQLError::Internal("prepared mutation action payload has no plan".into()))?;
    reject_unknown_fields(&fields, "prepared mutation action payload")?;
    match kind.as_str() {
        "insert" => {
            let Value::Map(mut fields) = plan else {
                return Err(SQLError::Internal(
                    "prepared insert action plan is not a map".into(),
                ));
            };
            let table = match fields.remove("table") {
                Some(Value::Str(table)) => table,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared insert action plan has no table".into(),
                    ))
                }
            };
            let doc_id = decode_prepared_doc_id(
                fields.remove("doc_id").ok_or_else(|| {
                    SQLError::Internal("prepared insert action plan has no document id".into())
                })?,
                "prepared insert action plan",
            )?;
            let document = match fields.remove("document") {
                Some(Value::Map(document)) => document,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared insert action plan has no document".into(),
                    ))
                }
            };
            reject_unknown_fields(&fields, "prepared insert action plan")?;
            Ok(PreparedMutationAction::Insert(PreparedDocumentInsert {
                table,
                doc_id,
                document,
            }))
        }
        "rewrite" => Ok(PreparedMutationAction::Rewrite(
            decode_prepared_document_rewrite(plan)?,
        )),
        "delete" => Ok(PreparedMutationAction::Delete(
            decode_prepared_document_delete(plan)?,
        )),
        _ => Err(SQLError::Internal(format!(
            "prepared mutation action payload has unknown kind `{kind}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(key: &str, value: i64) -> Document {
        BTreeMap::from([(key.to_string(), Value::Int(value))])
    }

    fn rewrite() -> PreparedDocumentRewrite {
        PreparedDocumentRewrite {
            table: "public.source".into(),
            doc_id: 7,
            destination: Some(("public.destination".into(), 9)),
            partition_move_delete: Some(Box::new(PreparedDocumentDelete {
                table: "public.source".into(),
                doc_id: 7,
                document: document("id", 7),
                actions: Vec::new(),
            })),
            old_document: document("value", 1),
            new_document: document("value", 2),
            actions: vec![PreparedDocumentRewrite {
                table: "public.child".into(),
                doc_id: 11,
                destination: None,
                partition_move_delete: None,
                old_document: document("parent", 7),
                new_document: document("parent", 9),
                actions: Vec::new(),
                trigger_updated_columns: None,
                capture_partition_move_update_transition: false,
            }],
            trigger_updated_columns: Some(vec!["value".into(), "status".into()]),
            capture_partition_move_update_transition: true,
        }
    }

    #[test]
    fn prepared_rewrite_and_delete_codec_round_trip_nested_actions() {
        let expected_rewrite = rewrite();
        let actual_rewrite = decode_prepared_document_rewrite(encode_prepared_document_rewrite(
            expected_rewrite.clone(),
        ))
        .unwrap();
        assert_eq!(actual_rewrite, expected_rewrite);

        let expected_delete = PreparedDocumentDelete {
            table: "public.parent".into(),
            doc_id: 3,
            document: document("id", 3),
            actions: vec![
                PreparedDeleteAction::Rewrite(Box::new(rewrite())),
                PreparedDeleteAction::Delete(Box::new(PreparedDocumentDelete {
                    table: "public.child".into(),
                    doc_id: 4,
                    document: document("id", 4),
                    actions: Vec::new(),
                })),
            ],
        };
        let actual_delete = decode_prepared_document_delete(encode_prepared_document_delete(
            expected_delete.clone(),
        ))
        .unwrap();
        assert_eq!(actual_delete, expected_delete);
    }

    #[test]
    fn prepared_codec_rejects_version_width_and_unknown_fields() {
        let mut wrong_version = match encode_prepared_document_rewrite(rewrite()) {
            Value::Map(fields) => fields,
            _ => unreachable!(),
        };
        wrong_version.insert("version".into(), Value::Int(2));
        assert!(decode_prepared_document_rewrite(Value::Map(wrong_version)).is_err());

        assert!(decode_prepared_doc_id(Value::Bytes(vec![0; 3]), "test identity").is_err());

        let mut unknown_field = match encode_prepared_document_delete(PreparedDocumentDelete {
            table: "public.items".into(),
            doc_id: 1,
            document: document("id", 1),
            actions: Vec::new(),
        }) {
            Value::Map(fields) => fields,
            _ => unreachable!(),
        };
        unknown_field.insert("unexpected".into(), Value::Null);
        assert!(decode_prepared_document_delete(Value::Map(unknown_field)).is_err());

        let mut invalid_trigger_columns = match encode_prepared_document_rewrite(rewrite()) {
            Value::Map(fields) => fields,
            _ => unreachable!(),
        };
        invalid_trigger_columns.insert(
            "trigger_updated_columns".into(),
            Value::List(vec![Value::Int(1)]),
        );
        assert!(decode_prepared_document_rewrite(Value::Map(invalid_trigger_columns)).is_err());
    }

    #[test]
    fn prepared_insert_conflict_codec_round_trips_every_action() {
        for expected in [
            PreparedInsertConflict::Unresolved,
            PreparedInsertConflict::Insert {
                doc_id: 23,
                supplied: true,
            },
            PreparedInsertConflict::Skip,
            PreparedInsertConflict::Updated(rewrite()),
        ] {
            let actual =
                decode_prepared_insert_conflict(encode_prepared_insert_conflict(expected.clone()))
                    .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn prepared_insert_conflict_codec_rejects_malformed_payloads() {
        assert!(decode_prepared_insert_conflict(Value::Str("skip".into())).is_err());

        let mut missing_width =
            match encode_prepared_insert_conflict(PreparedInsertConflict::Insert {
                doc_id: 23,
                supplied: false,
            }) {
                Value::Map(fields) => fields,
                _ => unreachable!(),
            };
        missing_width.insert("doc_id".into(), Value::Bytes(vec![0; 3]));
        assert!(decode_prepared_insert_conflict(Value::Map(missing_width)).is_err());

        let mut unknown_field = match encode_prepared_insert_conflict(PreparedInsertConflict::Skip)
        {
            Value::Map(fields) => fields,
            _ => unreachable!(),
        };
        unknown_field.insert("unexpected".into(), Value::Null);
        assert!(decode_prepared_insert_conflict(Value::Map(unknown_field)).is_err());
    }

    #[test]
    fn prepared_mutation_action_codec_round_trips_every_action() {
        for expected in [
            PreparedMutationAction::Insert(PreparedDocumentInsert {
                table: "public.items".into(),
                doc_id: 29,
                document: document("id", 29),
            }),
            PreparedMutationAction::Rewrite(rewrite()),
            PreparedMutationAction::Delete(PreparedDocumentDelete {
                table: "public.items".into(),
                doc_id: 31,
                document: document("id", 31),
                actions: Vec::new(),
            }),
        ] {
            let actual =
                decode_prepared_mutation_action(encode_prepared_mutation_action(expected.clone()))
                    .unwrap();
            assert_eq!(actual, expected);
        }
    }
}
