//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn table(name: &str, columns_json: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        role_owner: "uqa".into(),
        acl: None,
        column_acls: std::collections::BTreeMap::default(),
        object_id: [name.len() as u8; 16],
        storage_generation: [name.len() as u8 + 1; 16],
        analyzer_json: "{}".into(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: columns_json.into(),
        constraints_json: String::new(),
    }
}

#[test]
fn migration_42_separates_tuple_xmin_without_discarding_user_fields() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_table(&table("system_xmin", r#"[{"name":"value"}]"#))
        .unwrap();
    catalog
        .save_table(&table("schemaless_user_xmin", "[]"))
        .unwrap();
    catalog
        .save_table(&table("declared_user_xmin", r#"[{"name":"xmin"}]"#))
        .unwrap();
    drop(catalog);

    connection
        .with(|database| {
            for (table_name, doc_id, body) in [
                (
                    "public.system_xmin",
                    1,
                    serde_json::json!({
                        "value": 1,
                        "\0uqa.system.xmin": 17,
                        "xmin": 17,
                    }),
                ),
                (
                    "public.schemaless_user_xmin",
                    2,
                    serde_json::json!({
                        "value": 2,
                        "\0uqa.system.xmin": 18,
                        "\0uqa.user.xmin": true,
                        "xmin": 18,
                    }),
                ),
                (
                    "public.declared_user_xmin",
                    3,
                    serde_json::json!({
                        "\0uqa.system.xmin": 19,
                        "xmin": 19,
                    }),
                ),
            ] {
                database.execute(
                    "INSERT INTO _documents(table_name, doc_id, body, tuple_xmin) VALUES (?1, ?2, ?3, NULL)",
                    rusqlite::params![table_name, doc_id, serde_json::to_string(&body)?],
                )?;
            }
            database.execute(
                "UPDATE _metadata SET value = '41' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|database| {
            let mut statement = database
                .prepare("SELECT table_name, body, tuple_xmin FROM _documents ORDER BY doc_id")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].0, "public.system_xmin");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&rows[0].1)?,
                serde_json::json!({"value": 1})
            );
            assert_eq!(rows[0].2, 17);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&rows[1].1)?,
                serde_json::json!({"value": 2, "xmin": 18})
            );
            assert_eq!(rows[1].2, 18);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&rows[2].1)?,
                serde_json::json!({"xmin": 19})
            );
            assert_eq!(rows[2].2, 19);
            let version: String = database.query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_42_rewrites_multiple_document_pages() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_table(&table("paged_xmin", r#"[{"name":"value"}]"#))
        .unwrap();
    drop(catalog);

    connection
        .with(|database| {
            let mut insert = database.prepare_cached(
                "INSERT INTO _documents(table_name, doc_id, body, tuple_xmin) VALUES ('public.paged_xmin', ?1, ?2, NULL)",
            )?;
            for doc_id in 1_i64..=513 {
                let xmin = doc_id + 1000;
                let body = serde_json::json!({
                    "value": doc_id,
                    "\0uqa.system.xmin": xmin,
                    "xmin": xmin,
                });
                insert.execute(rusqlite::params![doc_id, serde_json::to_string(&body)?])?;
            }
            drop(insert);
            database.execute(
                "UPDATE _metadata SET value = '41' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|database| {
            let count: i64 = database.query_row(
                "SELECT COUNT(*) FROM _documents WHERE table_name = 'public.paged_xmin'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 513);
            let (body, tuple_xmin): (String, i64) = database.query_row(
                "SELECT body, tuple_xmin FROM _documents WHERE table_name = 'public.paged_xmin' AND doc_id = 513",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&body)?,
                serde_json::json!({"value": 513})
            );
            assert_eq!(tuple_xmin, 1513);
            Ok(())
        })
        .unwrap();
}
