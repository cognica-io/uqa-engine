//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{DecimalValue, EnumLabelKey, EnumValue, RecordFieldType, RowValue};

fn input_type(case: &serde_json::Value) -> ColumnType {
    match case["selected"]["input_name"].as_str().unwrap() {
        "anyarray" => ColumnType::Array(Box::new(ColumnType::Integer)),
        "anyenum" => ColumnType::Enum(crate::ast::EnumTypeReference {
            schema: "public".into(),
            name: "native_bound_enum".into(),
            oid: 90_201,
            array_oid: 90_202,
        }),
        "anyrange" => ColumnType::from_sql_name("int4range").unwrap(),
        "anymultirange" => ColumnType::from_sql_name("int4multirange").unwrap(),
        name => {
            let oid = case["selected"]["opcintype"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            let ty = type_metadata::builtin_scalar_type(oid)
                .cloned()
                .unwrap_or_else(|| ColumnType::from_sql_name(name).unwrap());
            assert_eq!(type_metadata::pg_type_oid(&ty), i64::from(oid), "{name}");
            ty
        }
    }
}

fn input(sql: &str, ty: &ColumnType) -> Value {
    if matches!(ty, ColumnType::Enum(_)) {
        let label = sql.strip_prefix("'").unwrap().split_once("'::").unwrap().0;
        let index = ["z", "a", "m"]
            .iter()
            .position(|candidate| *candidate == label)
            .unwrap();
        return Value::Enum(EnumValue::new(
            90_201,
            EnumLabelKey::initial(3).unwrap()[index].clone(),
        ));
    }
    if matches!(ty, ColumnType::Record) {
        let fields = sql
            .strip_prefix("ROW(")
            .unwrap()
            .split_once(")::")
            .unwrap()
            .0;
        let (integer, numeric) = fields.split_once(',').unwrap();
        return Value::Row(
            RowValue::typed(
                vec![
                    Value::Int(integer.parse().unwrap()),
                    Value::Decimal(DecimalValue::parse(numeric).unwrap()),
                ],
                [23, 1700]
                    .into_iter()
                    .map(|oid| RecordFieldType {
                        oid,
                        type_modifier: -1,
                        collation_oid: Some(0),
                    })
                    .collect(),
            )
            .unwrap(),
        );
    }
    let crate::Statement::Select(query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("reference scalar")
    };
    crate::expr::eval(
        &query.projections[0].expr,
        &crate::expr::EvalContext::new(None, &[]),
    )
    .unwrap()
}

#[test]
fn selected_native_btree_support_matches_implemented_fixed_builtin_reference_classes() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../native.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut classes = std::collections::BTreeSet::new();
    let mut observations = 0;
    for case in reference["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let ty = input_type(case);
        let class = case["selected"]["opcname"].as_str().unwrap();
        classes.insert(class);
        let binding = binding(&ty, class);
        let values = case["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|sql| input(sql.as_str().unwrap(), &ty))
            .collect::<Vec<_>>();
        let collation = case["collation"].as_str().map_or(100, |name| {
            crate::catalog::collations::builtin_collation_oid(name).unwrap() as u32
        });
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                let expected = case["orders"][i * values.len() + j]
                    .as_i64()
                    .unwrap()
                    .cmp(&0);
                let actual = compare_btree_inputs_with_catalog_and_control(
                    binding, left, right, collation, None, &control,
                )
                .unwrap_or_else(|error| panic!("{name}: {i}/{j}: {error}"));
                assert_eq!(actual, expected, "{name}: {i}/{j}");
                assert_eq!(memory.used(), 0, "{name}: {i}/{j}");
                observations += 1;
            }
        }
    }
    assert_eq!(classes.len(), 31);
    assert_eq!(observations, 832);
}
