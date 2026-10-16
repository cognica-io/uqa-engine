//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, DatumValue, RecordFieldType, RowValue,
};
use uqa_sql::{
    catalog::node_tree::{self, Field},
    expr::SQLValueCatalog,
    SQLError,
};
mod catalog;
use catalog::{integer, Catalog};

fn values(catalog: &Catalog, name: &str, index: usize) -> Vec<Value> {
    let node = &catalog.case(&format!("{name}_comparison_catalog"))["results"][0]["rows"][index][0];
    let Field::Node(node) = node_tree::parse(node.as_str().unwrap()).unwrap() else {
        panic!("constant")
    };
    let Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
        panic!("datum")
    };
    let Field::Atom(oid) = node.field("consttype").unwrap() else {
        panic!("OID")
    };
    let value = uqa_sql::expr::datums::read_with_value_catalog_and_control(
        &DatumValue::new(oid.parse().unwrap(), 0, bytes.clone()),
        Some(catalog),
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    let Value::Array(array) = value else {
        panic!("array")
    };
    array.elements().to_vec()
}
fn containers(value: &Value, oid: u32) -> [Value; 3] {
    [
        value.clone(),
        Value::Row(
            RowValue::typed(
                vec![value.clone()],
                vec![RecordFieldType {
                    oid,
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        ),
        Value::Array(
            ArrayValue::try_new(vec![value.clone()])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        ),
    ]
}

#[test]
fn declared_range_exact_hash_reservation_and_spill_keys_match_postgresql_equality() {
    let catalog = Catalog::new();
    let hasher = std::collections::hash_map::RandomState::new();
    let mut comparisons = 0;
    for name in [
        "enum_window",
        "domain_window",
        "text_window",
        "array_window",
        "record_window",
        "reverse_window",
    ] {
        for (index, column) in [(0, "r"), (1, "m")] {
            let oid = integer(&catalog.metadata(name)[if index == 0 { 1 } else { 6 }]);
            let inputs = values(&catalog, name, index);
            let expected = catalog.case(&format!("{name}_comparison_{column}"))["results"][0]
                ["rows"]
                .as_array()
                .unwrap();
            for row in expected {
                let i = row[0].as_str().unwrap().parse::<usize>().unwrap() - 1;
                let j = row[1].as_str().unwrap().parse::<usize>().unwrap() - 1;
                let equal = row[2] == "0";
                for (left, right) in containers(&inputs[i], oid)
                    .iter()
                    .zip(containers(&inputs[j], oid))
                {
                    let key = |value| {
                        encode_sql_key_with_enum_catalog(
                            std::iter::once(Some(value)),
                            Some(&catalog),
                        )
                        .unwrap()
                    };
                    assert_eq!(key(left) == key(&right), equal, "{name} {column} {i} {j}");
                    if equal {
                        let hash = |value| {
                            hash_canonical_row_with_enum_catalog(
                                &hasher,
                                std::iter::once(Some(value)),
                                Some(&catalog),
                            )
                            .unwrap()
                        };
                        assert_eq!(hash(left), hash(&right));
                        let locks = |value| {
                            reservations::canonical_row_lock_keys_with_catalog(
                                std::slice::from_ref(value),
                                &catalog,
                            )
                            .unwrap()
                        };
                        assert_eq!(locks(left)[0], locks(&right)[0]);
                    }
                    comparisons += 1;
                }
            }
            for budget in [1, 64 * 1024] {
                let directory = tempfile::tempdir().unwrap();
                let mut seen =
                    crate::distinct::ExactRowSet::with_spill_directory(budget, directory.path());
                for (index, value) in inputs.iter().enumerate() {
                    let new =
                        !(0..index).any(|prior| expected[prior * inputs.len() + index][2] == "0");
                    assert_eq!(
                        seen.insert_sql_values(std::slice::from_ref(value), Some(&catalog))
                            .unwrap(),
                        new
                    );
                }
                assert_eq!(seen.has_spilled(), budget == 1);
                drop(seen);
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            }
        }
    }
    assert_eq!(comparisons, 1764);
}

#[test]
fn declared_range_key_output_keeps_the_original_budget_and_releases_failure() {
    let catalog = Catalog::new();
    let inputs = values(&catalog, "record_window", 1);
    for limit in [0, 16, 64, 128, 512, 4096, 1 << 20] {
        let control = StorageReadControl::with_limit(limit);
        let mut output = CatalogKeyOutput {
            values: BudgetedOutput::new(&control),
            enums: Some(&catalog),
            sql_hash: true,
        };
        let result = encode_value(&inputs[6], &mut output);
        if limit == 1 << 20 {
            assert!(result.is_ok());
        } else if limit == 0 {
            assert!(result.is_err(), "zero allowance must reject output");
        }
        if let Err(error) = result {
            assert!(
                matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("53200")),
                "{error}"
            );
        }
        drop(output);
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    control.cancellation().cancel();
    let mut output = CatalogKeyOutput {
        values: BudgetedOutput::new(&control),
        enums: Some(&catalog),
        sql_hash: true,
    };
    let error = encode_value(&inputs[6], &mut output).unwrap_err();
    assert!(
        matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("57014")),
        "{error}"
    );
    drop(output);
    assert_eq!(control.memory().used(), 0);
}
