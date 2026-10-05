//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumLabelCatalog, EnumTypeLabel, EnumTypeLabels};
use std::sync::Arc;
use uqa_core::EnumLabelKey;

struct Labels(&'static str);

impl EnumLabelCatalog for Labels {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: vec![EnumTypeLabel {
                    oid: 16_386,
                    key: EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
                    label: self.0.into(),
                }],
            })
        }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(Some("color".into()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn retained_enum_inputs_keep_label_identity_and_array_bounds_after_rename() {
    let scalar = ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "color".into(),
        oid: 16_384,
        array_oid: 16_385,
    });
    for (sql, target) in [
        ("SELECT 'old'", scalar.clone()),
        (
            "SELECT '[0:1]={old,NULL}'",
            ColumnType::Array(Box::new(scalar)),
        ),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut parameters = ParameterTypes::with_input_constants(&[], None, Some(&Labels("old")));
        plan.visit_scalar_expressions(&mut |expression| {
            let ScalarExpr::Literal(Value::Str(text)) = expression else {
                panic!("literal");
            };
            parameters
                .coerce_unknown(
                    &mut ExpressionType::unknown_literal(expression, text.clone()),
                    &target,
                )
                .unwrap();
        });
        parameters.take_input_constants().apply(&mut plan).unwrap();
        plan.visit_scalar_expressions(&mut |expression| {
            let ScalarExpr::TypedLiteral { value, .. } = expression else {
                panic!("typed input");
            };
            let renamed =
                crate::expr::enums::render_enum_labels(Some(&Labels("new")), value).unwrap();
            match renamed {
                Value::Str(text) => assert_eq!(text, "new"),
                Value::Array(array) => {
                    assert_eq!(array.lower_bounds(), [0]);
                    assert_eq!(array.elements(), [Value::Str("new".into()), Value::Null]);
                }
                other => panic!("unexpected enum output: {other:?}"),
            }
        });
    }
}
