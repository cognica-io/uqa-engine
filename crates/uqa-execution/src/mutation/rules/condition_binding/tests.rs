//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::scalar::plan::{eval_physical, PhysicalEvalContext};
use uqa_sql::{
    ast::{ColumnType, Statement},
    catalog::events::RuleConditionBinding,
};

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    variables: Vec<Variable>,
}
#[derive(serde::Deserialize)]
struct Variable {
    side: String,
    attribute: usize,
    collation: u32,
}

#[test]
fn retained_rule_condition_inputs_preserve_postgresql_collations_and_values() {
    let references: Vec<Reference> =
        serde_json::from_str(include_str!("tests/collations.json")).unwrap();
    let names = ["c", "p", "plain", "n"].map(str::to_string).to_vec();
    let columns = names
        .iter()
        .enumerate()
        .map(|(position, name)| {
            (
                name.clone(),
                RuleColumnMetadata {
                    ty: if position == 3 {
                        ColumnType::Integer
                    } else {
                        ColumnType::Text
                    },
                    collation_oid: [Some(950), Some(951), None, None][position],
                    uses_document_id: false,
                    position,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let values = [
        Value::Str("AbC".into()),
        Value::Str("xyz".into()),
        Value::Str("plain".into()),
        Value::Int(42),
    ];
    for case in references {
        let Statement::CreateRule(rule) = uqa_sql::compile(&case.sql).unwrap().remove(0) else {
            panic!("rule")
        };
        let binding = RuleConditionBinding::for_event(&names, rule.event);
        let reference_column = |variable: &Variable| {
            let name = &names[variable.attribute - 1];
            if variable.side == "old" {
                binding.old_column(name)
            } else {
                binding.new_column(name)
            }
            .unwrap()
        };
        let required = case.variables.iter().map(reference_column).collect();
        for present in [true, false] {
            let document =
                present.then(|| names.iter().cloned().zip(values.iter().cloned()).collect());
            let row = RuleRowImage {
                old: document.clone(),
                new: document,
                ..RuleRowImage::empty()
            };
            let mut inputs = BTreeMap::new();
            let mut source = crate::mutation::rules::RuleInputProjection::default().source;
            append_row_values(
                &binding,
                &required,
                &row,
                &columns,
                &mut inputs,
                &mut source,
            )
            .unwrap();
            assert!(
                source.schema.columns().is_empty(),
                "runtime identities must remain private"
            );
            assert_eq!(source.schema.physical_width(), case.variables.len());
            for variable in &case.variables {
                let input = &inputs[&reference_column(variable)];
                let ScalarExpr::InternalColumn(column) = input.scalar else {
                    panic!("physical input")
                };
                assert_eq!(
                    source.schema.internal_collation(column),
                    Some(variable.collation),
                    "{}: {} attribute {}",
                    case.sql,
                    variable.side,
                    variable.attribute
                );
                let view = source.view();
                let context = PhysicalEvalContext::from_row_lookup(&view, &[])
                    .with_row_schema(&source.schema);
                assert_eq!(
                    eval_physical(input, &context).unwrap(),
                    if present {
                        values[variable.attribute - 1].clone()
                    } else {
                        Value::Null
                    }
                );
            }
        }
    }
}
