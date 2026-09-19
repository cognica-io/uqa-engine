//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View/foreign definitions and relation claims remain atomic across logical sessions.

use super::{open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_storage::{
    mvcc::VersionedSessionOptions, ForeignTableRow, RelationIdentity, TableAclEntry,
    TablePrivileges, ViewRow,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection};

fn legacy_security(
    row: &uqa_storage::RelationSecurityRow,
) -> &uqa_core::catalog_acl::LegacyRelationSecurity {
    let uqa_storage::RelationSecurityRow::Legacy(security) = row else {
        panic!("legacy fixture unexpectedly acquired role identities");
    };
    security
}

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("app", name)
}

fn acl() -> Vec<TableAclEntry> {
    vec![TableAclEntry {
        role: "reader".into(),
        grantor: Some("owner".into()),
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
    }]
}

fn view(name: &str, value: &str) -> ViewRow {
    ViewRow {
        relation: relation(name),
        security: uqa_storage::RelationSecurityRow::Legacy(
            uqa_core::catalog_acl::LegacyRelationSecurity {
                role_owner: "owner".into(),
                acl: Some(acl()),
                column_acls: BTreeMap::from([("field".into(), acl())]),
            },
        ),
        definition_json: value.into(),
    }
}

fn foreign(name: &str, server: &str, value: &str) -> ForeignTableRow {
    ForeignTableRow {
        relation: relation(name),
        security: uqa_storage::RelationSecurityRow::Legacy(
            uqa_core::catalog_acl::LegacyRelationSecurity {
                role_owner: "owner".into(),
                acl: Some(acl()),
                column_acls: BTreeMap::from([("field".into(), acl())]),
            },
        ),
        server_name: server.into(),
        columns_json: "[{\"name\":\"field\"}]".into(),
        options_json: value.into(),
    }
}

fn write(catalog: &Catalog, name: &str, value: &str) {
    catalog.save_foreign_server(name, "fixture", value).unwrap();
    catalog
        .save_view(&view(&format!("v_{name}"), value))
        .unwrap();
    catalog
        .save_foreign_table(&foreign(&format!("f_{name}"), name, value))
        .unwrap();
}

fn assert_value(catalog: &Catalog, name: &str, value: Option<&str>) {
    let views = catalog.load_views().unwrap();
    let tables = catalog.load_foreign_tables().unwrap();
    let servers = catalog.load_foreign_servers().unwrap();
    let v = views
        .iter()
        .find(|row| row.relation.name == format!("v_{name}"));
    let f = tables
        .iter()
        .find(|row| row.relation.name == format!("f_{name}"));
    assert_eq!(v.map(|row| row.definition_json.as_str()), value);
    assert_eq!(f.map(|row| row.options_json.as_str()), value);
    assert_eq!(
        servers
            .iter()
            .find(|row| row.0 == name)
            .map(|row| row.2.as_str()),
        value
    );
    if let Some(v) = v {
        assert_eq!(legacy_security(&v.security).role_owner, "owner");
        assert_eq!(legacy_security(&v.security).acl, Some(acl()));
        assert_eq!(
            legacy_security(&v.security).column_acls,
            BTreeMap::from([("field".into(), acl())])
        );
    }
    if let Some(f) = f {
        assert_eq!(legacy_security(&f.security).role_owner, "owner");
        assert_eq!(legacy_security(&f.security).acl, Some(acl()));
        assert_eq!(
            legacy_security(&f.security).column_acls,
            BTreeMap::from([("field".into(), acl())])
        );
        assert_eq!(f.columns_json, "[{\"name\":\"field\"}]");
        assert_eq!(f.server_name, name);
    }
}

#[test]
fn independent_native_relation_writers_commit_before_the_other_private_transaction_ends() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("relations.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog.save_schema("app").unwrap();
            bind(&connection);
            connection.begin_transaction().unwrap();
            write(&catalog, "a", "first");
            connection.savepoint("keep").unwrap();
            write(&catalog, "a", "second");
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let catalog = Catalog::open(connection.clone()).unwrap();
                connection.begin_transaction().unwrap();
                write(&catalog, "b", "committed");
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native relation writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_value(&catalog, "a", Some("second"));
            assert_value(&catalog, "b", None);
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    Some("second")
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    None
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    Some("first")
                }
            };
            drop(catalog);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let catalog = Catalog::open(reopened).unwrap();
            assert_value(&catalog, "a", expected);
            assert_value(&catalog, "b", Some("committed"));
        }
    }
}

fn assert_ordered_claims(catalog: &Catalog) {
    assert_eq!(
        catalog
            .load_foreign_servers()
            .unwrap()
            .iter()
            .map(|row| row.0.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "a\0suffix", "z"]
    );
    assert_eq!(
        catalog
            .load_views()
            .unwrap()
            .iter()
            .map(|row| row.relation.name.as_str())
            .collect::<Vec<_>>(),
        vec!["v_a", "v_a\0suffix", "v_z"]
    );
    assert_eq!(
        catalog
            .load_foreign_tables()
            .unwrap()
            .iter()
            .map(|row| row.relation.name.as_str())
            .collect::<Vec<_>>(),
        vec!["f_a", "f_a\0suffix", "f_z"]
    );
    assert!(catalog.save_view(&view("f_a", "collision")).is_err());
    assert!(catalog
        .save_foreign_table(&foreign("v_a", "a", "collision"))
        .is_err());
    assert!(!catalog.drop_view(&relation("f_a")).unwrap());
    catalog.drop_foreign_table(&relation("v_a")).unwrap();
    assert_value(catalog, "a", Some("a"));
    assert!(catalog.drop_schema("app").is_err());
    assert!(catalog
        .rename_view(&relation("v_a"), &relation("f_a"))
        .is_err());
    assert!(catalog
        .rename_foreign_table(&relation("f_a"), &relation("v_a"))
        .is_err());
    assert!(catalog
        .rename_view(&relation("v_a"), &relation("v_a"))
        .unwrap());
    assert!(catalog
        .rename_foreign_table(&relation("f_a"), &relation("f_a"))
        .unwrap());
    assert!(!catalog
        .rename_view(&relation("missing"), &relation("v_a"))
        .unwrap());
    assert!(!catalog
        .rename_foreign_table(&relation("missing"), &relation("f_a"))
        .unwrap());
}

#[test]
fn native_and_legacy_relation_lifecycle_preserve_claims_security_and_sorted_reads() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        catalog.save_schema("app").unwrap();
        if native {
            bind(&connection);
        }
        for name in ["z", "a", "a\0suffix"] {
            write(&catalog, name, name);
        }
        assert_ordered_claims(&catalog);
        connection.begin_transaction().unwrap();
        assert!(catalog
            .rename_view(&relation("v_a"), &relation("renamed_view"))
            .unwrap());
        assert!(catalog
            .rename_foreign_table(&relation("f_a"), &relation("renamed_foreign"))
            .unwrap());
        assert!(catalog
            .update_foreign_table_security(
                &relation("renamed_foreign"),
                &uqa_storage::RelationSecurityRow::legacy("bob")
            )
            .unwrap());
        let updated = catalog
            .load_foreign_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "renamed_foreign")
            .unwrap();
        assert_eq!(legacy_security(&updated.security).role_owner, "bob");
        assert_eq!(legacy_security(&updated.security).acl, None);
        assert!(legacy_security(&updated.security).column_acls.is_empty());
        assert_eq!(updated.options_json, "a");
        assert_eq!(updated.server_name, "a");
        assert_eq!(updated.columns_json, "[{\"name\":\"field\"}]");
        assert!(!catalog
            .update_foreign_table_security(
                &relation("missing"),
                &uqa_storage::RelationSecurityRow::legacy("bob")
            )
            .unwrap());
        // Both former names are available to a different kind inside this transaction.
        catalog.save_view(&view("f_a", "reused")).unwrap();
        catalog
            .save_foreign_table(&foreign("v_a", "a", "reused"))
            .unwrap();
        connection.rollback_transaction().unwrap();
        assert_value(&catalog, "a", Some("a"));
        connection.begin_transaction().unwrap();
        assert!(catalog
            .rename_view(&relation("v_a"), &relation("renamed_view"))
            .unwrap());
        assert!(catalog
            .rename_foreign_table(&relation("f_a"), &relation("renamed_foreign"))
            .unwrap());
        catalog.save_view(&view("f_a", "reused")).unwrap();
        catalog
            .save_foreign_table(&foreign("v_a", "a", "reused"))
            .unwrap();
        connection.commit_transaction().unwrap();
        let renamed = catalog
            .load_views()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "renamed_view")
            .unwrap();
        assert_eq!(renamed.definition_json, "a");
        assert_eq!(legacy_security(&renamed.security).acl, Some(acl()));
        assert_eq!(
            legacy_security(&renamed.security).column_acls,
            BTreeMap::from([("field".into(), acl())])
        );
        for row in catalog.load_views().unwrap() {
            assert!(catalog.drop_view(&row.relation).unwrap());
        }
        for row in catalog.load_foreign_tables().unwrap() {
            catalog.drop_foreign_table(&row.relation).unwrap();
        }
        for (name, _, _) in catalog.load_foreign_servers().unwrap() {
            catalog.drop_foreign_server(&name).unwrap();
        }
        assert!(catalog.load_foreign_servers().unwrap().is_empty());
        catalog.drop_schema("app").unwrap();
        assert!(catalog
            .save_view(&view("missing_schema", "failure"))
            .is_err());
        assert!(catalog
            .save_foreign_table(&foreign("missing_schema", "a", "failure"))
            .is_err());
        connection.begin_transaction().unwrap();
        catalog.save_schema("app").unwrap();
        write(&catalog, "private_schema", "visible");
        connection.commit_transaction().unwrap();
        assert_value(&catalog, "private_schema", Some("visible"));
    }
}

#[test]
fn converted_nullable_view_acls_survive_rename_and_other_session_replacement() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("app").unwrap();
    catalog.save_view(&view("legacy", "original")).unwrap();
    connection.with(|sqlite| {
        sqlite.execute("UPDATE _views SET acl_json = NULL, column_acls_json = NULL WHERE relation_name = 'legacy'", [])?;
        Ok(())
    }).unwrap();
    bind(&connection);
    let other = connection.new_session();
    let b = Catalog::open(other.clone()).unwrap();
    connection.begin_transaction().unwrap();
    assert!(b
        .rename_view(&relation("legacy"), &relation("renamed"))
        .unwrap());
    let renamed = b.load_views().unwrap().remove(0);
    assert_eq!(legacy_security(&renamed.security).acl, None);
    assert!(legacy_security(&renamed.security).column_acls.is_empty());
    other.with_physical(|sqlite| {
        let nulls: (bool, bool) = sqlite.query_row("SELECT acl_json IS NULL, column_acls_json IS NULL FROM _views WHERE relation_name = 'renamed'", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert_eq!(nulls, (true, true));
        Ok(())
    }).unwrap();
    b.save_view(&view("renamed", "replacement")).unwrap();
    let retained = catalog.load_views().unwrap().remove(0);
    assert_eq!(retained.relation.name, "legacy");
    assert_eq!(retained.definition_json, "original");
    assert_eq!(legacy_security(&retained.security).acl, None);
    assert!(legacy_security(&retained.security).column_acls.is_empty());
    connection.rollback_transaction().unwrap();
    let latest = catalog.load_views().unwrap().remove(0);
    assert_eq!(latest.relation.name, "renamed");
    assert_eq!(latest.definition_json, "replacement");
    assert_eq!(legacy_security(&latest.security).acl, Some(acl()));
}

#[test]
fn conflicting_relation_kinds_cannot_publish_the_same_name() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("app").unwrap();
    bind(&connection);
    let other = connection.new_session();
    let b = Catalog::open(other).unwrap();
    connection.begin_transaction().unwrap();
    catalog.save_view(&view("contended", "loser")).unwrap();
    b.save_foreign_table(&foreign("contended", "remote", "winner"))
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert!(catalog.load_views().unwrap().is_empty());
    assert_eq!(
        catalog.load_foreign_tables().unwrap()[0].options_json,
        "winner"
    );
    assert!(catalog
        .save_view(&view("contended", "still-conflicts"))
        .is_err());
    catalog.drop_foreign_table(&relation("contended")).unwrap();
    catalog.save_view(&view("contended", "reclaimed")).unwrap();
}

#[test]
fn rejected_definition_and_rename_batches_preserve_names_and_prior_rows() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("app").unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    catalog.save_view(&view("original", "kept")).unwrap();
    let oversized = "x".repeat(1024 * 1024);
    for explicit in [true, false] {
        if explicit {
            connection.begin_transaction().unwrap();
        }
        assert!(catalog.save_view(&view("original", &oversized)).is_err());
        assert!(catalog.save_view(&view("vacant", &oversized)).is_err());
        assert!(catalog
            .rename_view(&relation("original"), &relation(&oversized))
            .is_err());
        assert!(catalog
            .save_foreign_table(&foreign("foreign_vacant", "remote", &oversized))
            .is_err());
        assert_eq!(catalog.load_views().unwrap()[0].definition_json, "kept");
        assert_eq!(connection.in_transaction(), explicit);
        // Failure must not leave a claim that prevents another relation kind from using the name.
        catalog
            .save_foreign_table(&foreign("vacant", "remote", "small"))
            .unwrap();
        catalog.save_view(&view("foreign_vacant", "small")).unwrap();
        catalog.drop_foreign_table(&relation("vacant")).unwrap();
        assert!(catalog.drop_view(&relation("foreign_vacant")).unwrap());
        if explicit {
            connection.commit_transaction().unwrap();
        }
    }
}

#[test]
fn failed_relation_publication_rolls_back_claims_definitions_and_servers_before_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure.db");
        let connection = open(mode, &path);
        let catalog = Catalog::open(connection.clone()).unwrap();
        catalog.save_schema("app").unwrap();
        bind(&connection);
        let other = connection.new_session();
        let observer = Catalog::open(other.clone()).unwrap();
        connection.begin_transaction().unwrap();
        write(&catalog, "failed", "value");
        other.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER injected_relation_failure BEFORE INSERT ON _foreign_tables BEGIN SELECT RAISE(ABORT, 'injected relation failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_value(&observer, "failed", None);
        other
            .with_physical(|sqlite| {
                let claims: i64 = sqlite.query_row(
                    "SELECT count(*) FROM _relations WHERE schema_name = 'app'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(claims, 0);
                sqlite.execute_batch("DROP TRIGGER injected_relation_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_value(&observer, "failed", Some("value"));
        drop(observer);
        drop(other);
        drop(catalog);
        drop(connection);
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_value(&Catalog::open(reopened).unwrap(), "failed", Some("value"));
    }
}
