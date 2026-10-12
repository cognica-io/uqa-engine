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
