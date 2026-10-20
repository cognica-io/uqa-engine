//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod collations;

struct Row(crate::ResultRow);

impl RuleRowValues for Row {
    fn old_row(&self) -> Option<&crate::ResultRow> {
        Some(&self.0)
    }
    fn new_row(&self) -> Option<&crate::ResultRow> {
        Some(&self.0)
    }
    fn old_doc_id(&self) -> Option<uqa_core::DocId> {
        None
    }
    fn new_doc_id(&self) -> Option<uqa_core::DocId> {
        None
    }
}

#[test]
fn rule_row_sources_preserve_every_admitted_byte() {
    let columns = BTreeMap::from([(
        "c".into(),
        RuleColumnMetadata {
            collation_oid: None,
            ty: ColumnType::InternalChar,
            uses_document_id: false,
            position: 0,
        },
    )]);
    for byte in 0..=u8::MAX {
        let signed = i64::from(i8::from_ne_bytes([byte]));
        let value =
            crate::expr::cast_value_from(&Value::Int(signed), "\"char\"", Some("integer")).unwrap();
        let row = Row(BTreeMap::from([("c".into(), value.clone())]));
        let source = rule_row_source(&[0], &[row], &columns).unwrap();
        let FromClause::Values { rows, .. } = source.clause else {
            unreachable!()
        };
        for field in &rows[0][..2] {
            let actual =
                crate::expr::eval(field, &crate::expr::EvalContext::new(None, &[])).unwrap();
            assert_eq!(actual, value);
            assert_eq!(
                crate::expr::cast_value_from(&actual, "integer", Some("\"char\"")).unwrap(),
                Value::Int(signed)
            );
        }
    }
}

#[test]
fn retained_rule_layout_keeps_declared_width_collation_and_whole_row_order() {
    let columns = BTreeMap::from([
        (
            "z".into(),
            RuleColumnMetadata {
                ty: ColumnType::SmallInteger,
                collation_oid: None,
                uses_document_id: false,
                position: 0,
            },
        ),
        (
            "a".into(),
            RuleColumnMetadata {
                ty: ColumnType::Text,
                collation_oid: Some(950),
                uses_document_id: false,
                position: 1,
            },
        ),
    ]);
    let row = Row(BTreeMap::from([
        ("z".into(), Value::Int(7)),
        ("a".into(), Value::Null),
    ]));
    let input = RetainedRuleInput::new(&columns);
    assert_eq!(input.schema().physical_width(), 7);
    assert!(input.schema().columns().is_empty());
    assert_eq!(
        input.schema().internal_type(input.relation().column(2)),
        Some(&ColumnType::SmallInteger)
    );
    assert_eq!(
        input
            .schema()
            .internal_collation(input.relation().column(0)),
        Some(950)
    );
    let fields = input.schema().physical_record_fields(4).unwrap();
    assert_eq!(
        fields.as_ref(),
        &[Some(ColumnType::SmallInteger), Some(ColumnType::Text)]
    );
    let values = input.row_values(&row, 9).unwrap();
    assert_eq!(
        &values[..4],
        &[Value::Null, Value::Null, Value::Int(7), Value::Int(7)]
    );
    let Value::Record(fields) = &values[4] else {
        panic!("whole OLD record")
    };
    assert_eq!(
        fields.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(values[6], Value::Int(9));
    assert_eq!(
        input.admitted_types().as_ref(),
        &[
            Some("pg_catalog.\"text\"".into()),
            Some("pg_catalog.\"text\"".into()),
            Some("pg_catalog.\"int2\"".into()),
            Some("pg_catalog.\"int2\"".into()),
            None,
            None,
            None
        ]
    );
}

#[test]
fn retained_rule_input_rejects_an_unrelated_source_before_action_binding() {
    let columns = BTreeMap::new();
    let input = RetainedRuleInput::new(&columns);
    let other = RetainedRuleInput::new(&columns);
    let descriptor =
        crate::ast::RetainedRowSource::new(other.relation(), other.schema().clone(), 1).unwrap();
    assert!(input
        .bind_action(descriptor, &|_| panic!("unrelated row owner"))
        .is_err());
}
