//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn range_peers_preserve_selected_types_in_memory_and_spill() {
    for kind in ["numrange", "nummultirange"] {
        let ty = ColumnType::from_sql_name(kind).unwrap();
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for ty in [ty, domain, ColumnType::Text] {
            for budget in [1, 4096] {
                let types = [Some(ty.clone()), Some(ColumnType::Integer)];
                let schema = RowSchema::with_types(vec!["r".into(), "n".into()], types.to_vec());
                let mut partition = BufferedIndexedSpill::new(schema, budget);
                for (input, n) in [
                    (Some("[1.0,3.00)"), 1),
                    (Some("[1.00,3.0)"), 1),
                    (Some("[1.00,3.0)"), 2),
                    (Some("[4,5)"), 1),
                    (None, 1),
                    (None, 1),
                ] {
                    let value = input.map_or(Value::Null, |input| {
                        let input = if kind == "nummultirange" {
                            format!("{{{input}}}")
                        } else {
                            input.into()
                        };
                        uqa_sql::expr::cast_value(&Value::Str(input), kind).unwrap()
                    });
                    partition
                        .push(&PhysicalRow::from_values(vec![value, Value::Int(n)]))
                        .unwrap();
                }
                assert_eq!(partition.spilled_bytes() > 0, budget == 1);
                let orders = ["r", "n"].map(|name| ScalarOrder {
                    expr: ScalarExpr::Column(name.into()),
                    descending: false,
                    nulls: None,
                });
                let arena = PlanSubqueryArena::new(&[], None);
                let mut rows =
                    PartitionRows::new(&mut partition, &orders, &types, &[], &NoSequences, &arena)
                        .unwrap();
                assert_eq!(rows.are_peers(0, 1).unwrap(), ty != ColumnType::Text);
                assert!(!rows.are_peers(1, 2).unwrap());
                assert!(!rows.are_peers(2, 3).unwrap());
                assert!(!rows.are_peers(3, 4).unwrap());
                assert!(rows.are_peers(4, 5).unwrap());
                assert!(rows.are_peers(0, 0).unwrap());
            }
        }
    }
}
