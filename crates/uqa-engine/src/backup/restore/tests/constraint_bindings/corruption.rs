//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{Statement, TableConstraintSet};

fn expression(sql: &str) -> Expr {
    let Statement::CreateTable(mut table) =
        uqa_sql::compile(&format!("CREATE TABLE items(id integer, CHECK({sql}))"))
            .unwrap()
            .remove(0)
    else {
        panic!("table CHECK")
    };
    table.checks.remove(0).expr
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_constraint_bindings_reject_table_foreign_and_domain_corruption(
    #[case] provider: Provider,
) {
    for (kind, condition, expected) in [
        ("table", "id", "boolean"),
        ("foreign_column", "id", "boolean"),
        ("foreign_table", "id", "boolean"),
        ("domain", "value", "boolean"),
        ("table", "true AND id", "boolean"),
        ("table", "'invalid_boolean' COLLATE \"C\"", "boolean"),
        (
            "foreign_table",
            "'invalid_boolean' COLLATE \"C\"",
            "boolean",
        ),
        ("domain", "'invalid_boolean' COLLATE \"C\"", "boolean"),
        ("table", "sum(id) > 0", "aggregate"),
        ("table", "absent_guard(id)", "absent_guard"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = provider.open(&directory.path().join("source"));
        source.sql("CREATE TABLE items(id integer, CONSTRAINT positive CHECK(id>0)); CREATE DOMAIN positive AS integer CHECK(VALUE>0); CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote_items(id integer CHECK(id>0), CONSTRAINT positive CHECK(id>0)) SERVER remote", &[]).unwrap();
        let bad = expression(condition);
        let root = if let Some(persistent) = source.storage.provider.clone() {
            drop(source);
            let session = persistent.open_session().unwrap();
            match kind {
                "table" => {
                    let mut row = session
                        .catalog
                        .load_tables()
                        .unwrap()
                        .into_iter()
                        .find(|row| row.relation.name == "items")
                        .unwrap();
                    let mut constraints: TableConstraintSet =
                        serde_json::from_str(&row.constraints_json).unwrap();
                    constraints.checks[0].expr = bad;
                    row.constraints_json = serde_json::to_string(&constraints).unwrap();
                    session.catalog.save_table(&row).unwrap();
                }
                "domain" => {
                    let key = "uqa.sql.domain.v1:public.positive";
                    let json = session.catalog.get_metadata(key).unwrap().unwrap();
                    let mut domain: uqa_sql::catalog::domain::StoredDomain =
                        serde_json::from_str(&json).unwrap();
                    domain.definition.checks[0].expression = bad;
                    session
                        .catalog
                        .set_metadata(key, &serde_json::to_string(&domain).unwrap())
                        .unwrap();
                }
                _ => {
                    let mut row = session.catalog.load_foreign_tables().unwrap().remove(0);
                    let mut value: serde_json::Value =
                        serde_json::from_str(&row.columns_json).unwrap();
                    if kind == "foreign_column" {
                        value["columns"][0]["check"] = serde_json::to_value(&bad).unwrap();
                    } else {
                        value["checks"][0]["expr"] = serde_json::to_value(&bad).unwrap();
                    }
                    row.columns_json = serde_json::to_string(&value).unwrap();
                    session.catalog.save_foreign_table(&row).unwrap();
                }
            }
            drop(session);
            capture_provider(persistent.as_ref(), &repository)
        } else {
            match kind {
                "table" => {
                    source
                        .storage
                        .tables
                        .read()
                        .get(&RelationIdentity::new("public", "items"))
                        .unwrap()
                        .table_checks
                        .write()
                        .edit(|checks| {
                            checks[0].expr = bad;
                            Ok(())
                        })
                        .unwrap();
                }
                "domain" => {
                    let mut domains = source.durable.domains.write();
                    let mut domain = domains.get("public.positive").unwrap().clone();
                    domain.definition.checks[0].expression = bad;
                    domains.insert("public.positive".into(), domain).unwrap();
                }
                _ => {
                    let name = RelationIdentity::new("public", "remote_items");
                    let mut catalog = source.durable.foreign_catalog.write();
                    let mut table = catalog.tables().get(&name).unwrap().clone();
                    if kind == "foreign_column" {
                        table.columns[0].check = Some(bad);
                    } else {
                        table.checks[0].expr = bad;
                    }
                    catalog.replace_table(name, table).unwrap();
                }
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
            expected,
        );
    }
}
