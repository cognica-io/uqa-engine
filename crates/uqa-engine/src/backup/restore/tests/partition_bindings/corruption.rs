//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::PartitionRangeDatum;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_partition_bindings_reject_corrupt_siblings_and_detached_conditions(
    #[case] provider: Provider,
) {
    for kind in [
        "list_overlap",
        "list_expression",
        "range_overlap",
        "range_width",
        "range_null",
        "range_key_order",
        "multirange_key_order",
        "hash_chain",
        "hash_overlap",
        "hash_zero",
        "detached",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = provider.open(&directory.path().join("source"));
        let declaration = if kind == "range_key_order" {
            "CREATE TABLE parent(id int4range) PARTITION BY RANGE(id); CREATE TABLE child PARTITION OF parent FOR VALUES FROM('[2,3)') TO('[10,11)')"
        } else if kind == "multirange_key_order" {
            "CREATE TABLE parent(id int4multirange) PARTITION BY RANGE(id); CREATE TABLE child PARTITION OF parent FOR VALUES FROM('{[2,3)}') TO('{[10,11)}')"
        } else if kind.starts_with("list") {
            "CREATE TABLE parent(id integer) PARTITION BY LIST(id); CREATE TABLE sibling PARTITION OF parent FOR VALUES IN(1); CREATE TABLE child PARTITION OF parent FOR VALUES IN(2)"
        } else if kind == "hash_chain" {
            "CREATE TABLE parent(id integer) PARTITION BY HASH(id); CREATE TABLE sibling PARTITION OF parent FOR VALUES WITH(MODULUS 4,REMAINDER 0); CREATE TABLE child PARTITION OF parent FOR VALUES WITH(MODULUS 4,REMAINDER 1)"
        } else if kind.starts_with("hash") {
            "CREATE TABLE parent(id integer) PARTITION BY HASH(id); CREATE TABLE sibling PARTITION OF parent FOR VALUES WITH(MODULUS 2,REMAINDER 0); CREATE TABLE child PARTITION OF parent FOR VALUES WITH(MODULUS 2,REMAINDER 1)"
        } else {
            "CREATE TABLE parent(id integer) PARTITION BY RANGE(id); CREATE TABLE sibling PARTITION OF parent FOR VALUES FROM(0) TO(10); CREATE TABLE child PARTITION OF parent FOR VALUES FROM(10) TO(20)"
        };
        source.sql(declaration, &[]).unwrap();
        if kind == "detached" {
            source
                .sql(
                    "ALTER TABLE parent DETACH PARTITION child CONCURRENTLY",
                    &[],
                )
                .unwrap();
        }
        let name = RelationIdentity::new("public", "child");
        let catalog = source
            .restored_catalog_read_view_with_control(
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )
            .unwrap();
        let table = catalog.snapshot().tables.get(&name).unwrap();
        let mut hierarchy = (*table.hierarchy).clone();
        let mut checks = table.checks.to_vec();
        let point = |value| PartitionRangeDatum::Value(Expr::Literal(value));
        match kind {
            "range_key_order" | "multirange_key_order" => {
                let (lower, upper) = if kind == "range_key_order" {
                    ("[10,11)", "[2,3)")
                } else {
                    ("{[10,11)}", "{[2,3)}")
                };
                hierarchy.partition_bound = Some(PartitionBound::Range {
                    lower: vec![point(Value::Str(lower.into()))],
                    upper: vec![point(Value::Str(upper.into()))],
                });
            }
            "list_overlap" => {
                hierarchy.partition_bound =
                    Some(PartitionBound::List(vec![Expr::Literal(Value::Int(1))]));
            }
            "list_expression" => {
                hierarchy.partition_bound =
                    Some(PartitionBound::List(vec![Expr::Column("id".into())]));
            }
            "range_overlap" => {
                hierarchy.partition_bound = Some(PartitionBound::Range {
                    lower: vec![point(Value::Int(5))],
                    upper: vec![point(Value::Int(15))],
                });
            }
            "range_width" => {
                hierarchy.partition_bound = Some(PartitionBound::Range {
                    lower: vec![],
                    upper: vec![point(Value::Int(20))],
                });
            }
            "range_null" => {
                hierarchy.partition_bound = Some(PartitionBound::Range {
                    lower: vec![point(Value::Null)],
                    upper: vec![point(Value::Int(20))],
                });
            }
            "hash_chain" => {
                hierarchy.partition_bound = Some(PartitionBound::Hash {
                    modulus: 6,
                    remainder: 1,
                });
            }
            "hash_overlap" => {
                hierarchy.partition_bound = Some(PartitionBound::Hash {
                    modulus: 4,
                    remainder: 0,
                });
            }
            "hash_zero" => {
                hierarchy.partition_bound = Some(PartitionBound::Hash {
                    modulus: 0,
                    remainder: 0,
                });
            }
            "detached" => {
                let check = checks
                    .iter_mut()
                    .find(|check| check.partition_constraint.is_some())
                    .unwrap();
                // A valid rendered CHECK cannot hide a corrupt retained routing predicate.
                check.expr = Expr::Literal(Value::Bool(true));
                check.partition_constraint.as_mut().unwrap().bound = PartitionBound::Range {
                    lower: vec![point(Value::Bool(true))],
                    upper: vec![point(Value::Int(20))],
                };
            }
            _ => unreachable!(),
        }
        drop(catalog);
        let root = if let Some(persistent) = source.storage.provider.clone() {
            drop(source);
            let session = persistent.open_session().unwrap();
            let mut table = session
                .catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|table| table.relation == name)
                .unwrap();
            let mut constraints: TableConstraintSet =
                serde_json::from_str(&table.constraints_json).unwrap();
            constraints.hierarchy = hierarchy;
            constraints.checks = checks;
            table.constraints_json = serde_json::to_string(&constraints).unwrap();
            session.catalog.save_table(&table).unwrap();
            drop(session);
            capture_provider(persistent.as_ref(), &repository)
        } else {
            {
                let tables = source.storage.tables.read();
                let table = tables.get(&name).unwrap();
                table
                    .hierarchy
                    .write()
                    .edit(|value| {
                        *value = hierarchy;
                        Ok(())
                    })
                    .unwrap();
                table
                    .table_checks
                    .write()
                    .edit(|value| {
                        *value = checks;
                        Ok(())
                    })
                    .unwrap();
            }
            let root = capture(&source, &repository, None);
            drop(source);
            root
        };
        reject_archive(
            provider,
            &repository,
            root,
            &directory.path().join("restored"),
            if kind == "hash_chain" {
                "divisibility chain"
            } else {
                "partition"
            },
        );
    }
}
