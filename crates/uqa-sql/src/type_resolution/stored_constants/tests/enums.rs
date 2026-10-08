//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumTypeLabel, EnumTypeLabels};
use std::sync::Arc;
use uqa_core::EnumLabelKey;

struct Labels(&'static str);

fn enum_type() -> ColumnType {
    ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "mood".into(),
        oid: 16_384,
        array_oid: 16_385,
    })
}

impl EnumLabelCatalog for Labels {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: ["sad", self.0]
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| EnumTypeLabel {
                        oid: 16_386 + u32::try_from(index).unwrap() * 2,
                        key: EnumLabelKey::from_bytes(vec![u8::try_from(index + 1).unwrap()])
                            .unwrap(),
                        label: label.into(),
                    })
                    .collect(),
            })
        }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(Some("mood".into()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

impl FunctionTypeResolver for Labels {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok((name == "enum#16384").then(enum_type))
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

#[test]
fn stored_membership_keeps_enum_labels_through_rebinding_and_rename() {
    let schema = RowSchema::with_types(vec!["m".into()], vec![Some(enum_type())]);
    for sql in ["m IN ('sad', 'happy')", "m NOT IN ('sad', m, 'happy')"] {
        let mut stored = expression(sql);
        assert!(store_operand_coercions(&mut stored, &schema, &[], &Labels("happy")).unwrap());
        let mut labels = Vec::new();
        stored.visit(&mut |node| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Enum(value),
                ..
            } = node
            {
                labels.push(
                    crate::expr::enums::enum_label_text(Some(&Labels("glad")), value).unwrap(),
                );
            }
        });
        assert_eq!(labels, ["sad", "glad"], "{sql}");
        let original = stored.clone();
        assert!(!store_operand_coercions(&mut stored, &schema, &[], &Labels("glad")).unwrap());
        assert!(!fold_stored_enum_constants(&mut stored, &schema, &[], &Labels("glad")).unwrap());
        assert_eq!(stored, original);
    }
}
