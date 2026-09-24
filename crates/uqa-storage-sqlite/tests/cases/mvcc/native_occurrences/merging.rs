//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{bind, fields, index, memory, open, MODES};
use uqa_storage::InvertedIndex;
use uqa_storage_sqlite::{Catalog, SQLiteInvertedIndex};

#[test]
fn native_occurrence_command_refresh_preserves_private_counts_and_savepoints() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("commands.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone())
            .unwrap()
            .save_table(&super::super::native_tables::schema("commands", 1, 1))
            .unwrap();
        bind(&connection);
        let mut a = index(&connection, "public.commands");
        a.add_document(1, fields("alpha")).unwrap();
        let other = open(mode, &path);
        bind(&other);
        let mut b = index(&other, "public.commands");
        connection.begin_transaction().unwrap();
        a.add_document(2, fields("alpha alpha")).unwrap();
        connection.savepoint("before").unwrap();
        let retained = a.snapshot().unwrap();
        b.add_document(3, fields("alpha alpha alpha")).unwrap();
        connection
            .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
            .unwrap();
        assert_eq!(a.total_field_length("body").unwrap(), 6);
        a.add_document(4, fields("alpha alpha alpha alpha"))
            .unwrap();
        connection.rollback_to_savepoint("before").unwrap();
        assert_eq!(a.total_field_length("body").unwrap(), 3);
        connection
            .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
            .unwrap();
        assert_eq!(a.total_field_length("body").unwrap(), 6);
        b.add_document(5, fields("alpha")).unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(retained.total_field_length("body").unwrap(), 3);
        assert_eq!(b.total_field_length("body").unwrap(), 7);
        assert_eq!(b.doc_count().unwrap(), 4);
        drop((a, b, retained, connection, other));
        let reopened = open(mode, &path);
        bind(&reopened);
        let index = index(&reopened, "public.commands");
        assert_eq!(index.total_field_length("body").unwrap(), 7);
        assert_eq!(index.doc_count().unwrap(), 4);
    }
}

#[test]
fn independent_documents_merge_in_one_native_occurrence_cluster() {
    for seeded in [false, true] {
        let connection = memory();
        Catalog::open(connection.clone())
            .unwrap()
            .save_table(&super::super::native_tables::schema("docs", 1, 1))
            .unwrap();
        let mut a = index(&connection, "public.docs");
        if seeded {
            a.add_document(1, fields("alpha")).unwrap();
        }
        let other = connection.new_session();
        let mut b = index(&other, "public.docs");
        connection.begin_transaction().unwrap();
        a.add_document(2, fields("alpha alpha")).unwrap();
        let retained = a.snapshot().unwrap();
        b.add_document(3, fields("alpha alpha alpha")).unwrap();
        assert!(connection.in_transaction());
        connection.commit_transaction().unwrap();
        assert_eq!(a.doc_count().unwrap(), if seeded { 3 } else { 2 });
        assert_eq!(
            a.total_field_length("body").unwrap(),
            if seeded { 6 } else { 5 }
        );
        assert_eq!(a.get_term_freq(2, "body", "alpha").unwrap(), 2);
        assert_eq!(a.get_term_freq(3, "body", "alpha").unwrap(), 3);
        assert_eq!(retained.doc_count().unwrap(), if seeded { 2 } else { 1 });
        assert_eq!(
            retained.total_field_length("body").unwrap(),
            if seeded { 3 } else { 2 }
        );
    }
}

fn verify(index: &SQLiteInvertedIndex, expected: &[(u64, &str)]) {
    assert_eq!(index.doc_count().unwrap(), expected.len() as u64);
    let length = expected
        .iter()
        .map(|(_, text)| text.split_whitespace().count() as u64)
        .sum::<u64>();
    assert_eq!(index.total_field_length("body").unwrap(), length);
    for id in [1, 2, 999] {
        let text = expected
            .iter()
            .find(|(doc, _)| *doc == id)
            .map_or("", |(_, text)| *text);
        assert_eq!(
            index.get_doc_length(id, "body").unwrap(),
            text.split_whitespace().count() as u64
        );
        for term in ["alpha", "beta", "gamma"] {
            assert_eq!(
                index.get_term_freq(id, "body", term).unwrap(),
                text.split_whitespace().filter(|word| *word == term).count() as u64
            );
        }
    }
    for term in ["alpha", "beta", "gamma"] {
        assert_eq!(
            index.doc_freq("body", term).unwrap(),
            expected
                .iter()
                .filter(|(_, text)| text.split_whitespace().any(|word| word == term))
                .count() as u64
        );
    }
}

#[test]
fn merged_native_occurrence_replacements_and_deletions_survive_all_file_modes() {
    for mode in MODES {
        for (seeded, left, right) in [
            (false, Some("alpha alpha"), Some("alpha alpha alpha")),
            (true, Some("alpha beta"), Some("alpha gamma gamma")),
            (true, None, None),
            (true, None, Some("alpha alpha")),
            (true, Some("beta beta"), Some("gamma")),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("merge.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            let mut a = index(&connection, "docs\0日本語");
            a.clear().unwrap();
            if seeded {
                a.try_add_documents(vec![(1, fields("alpha")), (2, fields("alpha alpha"))])
                    .unwrap();
            }
            let other = open(mode, &path);
            bind(&other);
            let mut b = index(&other, "docs\0日本語");
            connection.begin_transaction().unwrap();
            if let Some(text) = left {
                a.add_document(1, fields(text)).unwrap();
            } else {
                a.remove_document(1).unwrap();
            }
            connection.savepoint("kept").unwrap();
            let retained = a.snapshot().unwrap();
            a.add_document(999, fields("discarded")).unwrap();
            connection.rollback_to_savepoint("kept").unwrap();
            other.begin_transaction().unwrap();
            if let Some(text) = right {
                b.add_document(2, fields(text)).unwrap();
            } else {
                b.remove_document(2).unwrap();
            }
            other.commit_transaction().unwrap();
            assert!(connection.in_transaction());
            let private_count = retained.doc_count().unwrap();
            let private_length = retained.total_field_length("body").unwrap();
            connection.commit_transaction().unwrap();
            let expected = left
                .map(|text| (1, text))
                .into_iter()
                .chain(right.map(|text| (2, text)))
                .collect::<Vec<_>>();
            verify(&a, &expected);
            assert_eq!(retained.doc_count().unwrap(), private_count);
            assert_eq!(retained.total_field_length("body").unwrap(), private_length);
            drop((a, b, retained, other, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            verify(&index(&reopened, "docs\0日本語"), &expected);
        }
    }
}

#[test]
fn different_fields_on_the_same_absent_native_document_conflict() {
    let connection = memory();
    let mut a = index(&connection, "docs");
    a.clear().unwrap();
    let other = connection.new_session();
    let mut b = index(&other, "docs");
    connection.begin_transaction().unwrap();
    a.add_document(7, fields("alpha")).unwrap();
    b.add_document(
        7,
        std::collections::BTreeMap::from([("title".into(), "beta beta".into())]),
    )
    .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(a.get_doc_length(7, "body").unwrap(), 0);
    assert_eq!(a.get_doc_length(7, "title").unwrap(), 2);
}

#[test]
fn native_occurrence_clear_and_rebuild_fence_both_commit_orders() {
    for seeded in [false, true] {
        for structural_first in [false, true] {
            for rebuild in [false, true] {
                let connection = memory();
                let mut a = index(&connection, "docs");
                a.clear().unwrap();
                if seeded {
                    a.add_document(1, fields("alpha")).unwrap();
                }
                let other = connection.new_session();
                let mut b = index(&other, "docs");
                connection.begin_transaction().unwrap();
                a.add_document(2, fields("alpha alpha")).unwrap();
                other.begin_transaction().unwrap();
                if rebuild {
                    b.try_rebuild_documents(if seeded {
                        vec![(1, fields("alpha"))]
                    } else {
                        vec![]
                    })
                    .unwrap();
                } else {
                    b.clear().unwrap();
                }
                if structural_first {
                    other.commit_transaction().unwrap();
                    assert!(connection.commit_transaction().is_err());
                    connection.rollback_transaction().unwrap();
                    assert_eq!(a.get_doc_length(2, "body").unwrap(), 0);
                } else {
                    connection.commit_transaction().unwrap();
                    assert!(other.commit_transaction().is_err());
                    other.rollback_transaction().unwrap();
                    assert_eq!(b.get_doc_length(2, "body").unwrap(), 2);
                }
            }
        }
    }
}

#[test]
fn native_occurrence_writers_conflict_with_catalog_lifecycle_in_both_orders() {
    for action in [
        "drop",
        "drop-data",
        "purge",
        "rename",
        "drop-column",
        "rename-column",
    ] {
        for structural_first in [false, true] {
            let connection = memory();
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog
                .save_table(&super::super::native_tables::schema("docs", 1, 1))
                .unwrap();
            let mut a = index(&connection, "public.docs");
            a.add_document(1, fields("alpha")).unwrap();
            let other = connection.new_session();
            let other_catalog = Catalog::open(other.clone()).unwrap();
            connection.begin_transaction().unwrap();
            a.add_document(2, fields("alpha alpha")).unwrap();
            other.begin_transaction().unwrap();
            match action {
                "drop" => other_catalog.drop_table("public.docs").unwrap(),
                "drop-data" => {
                    other_catalog.drop_table_and_data("public.docs").unwrap();
                    other_catalog
                        .save_table(&super::super::native_tables::schema("docs", 2, 2))
                        .unwrap();
                }
                "purge" => other_catalog.purge_table_data("public.docs").unwrap(),
                "rename" => other_catalog
                    .rename_table_data("public.docs", "public.renamed")
                    .unwrap(),
                "drop-column" => other_catalog
                    .drop_column_data("public.docs", "body")
                    .unwrap(),
                _ => other_catalog
                    .rename_column_data("public.docs", "body", "title")
                    .unwrap(),
            }
            if structural_first {
                other.commit_transaction().unwrap();
                assert!(connection.commit_transaction().is_err(), "{action}");
                connection.rollback_transaction().unwrap();
                assert_eq!(a.get_doc_length(2, "body").unwrap(), 0);
            } else {
                connection.commit_transaction().unwrap();
                assert!(other.commit_transaction().is_err(), "{action}");
                other.rollback_transaction().unwrap();
                assert_eq!(
                    index(&other, "public.docs")
                        .get_doc_length(2, "body")
                        .unwrap(),
                    2
                );
            }
        }
    }
}

#[test]
fn retained_occurrence_guards_do_not_make_empty_table_data_collide() {
    let connection = memory();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_table(&super::super::native_tables::schema("docs", 1, 1))
        .unwrap();
    for name in ["docs", "renamed", "public.docs"] {
        let mut index = index(&connection, name);
        index
            .add_document(1, std::collections::BTreeMap::new())
            .unwrap();
        assert_eq!(index.doc_count().unwrap(), 0);
    }
    catalog.rename_table_data("docs", "renamed").unwrap();
    let mut renamed = index(&connection, "renamed");
    renamed.add_document(1, fields("beta beta")).unwrap();
    verify(&renamed, &[(1, "beta beta")]);
}
