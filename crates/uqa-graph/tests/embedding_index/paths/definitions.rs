//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition reads conflict with matching evaluated catalog changes, including absent names.

use super::{begin, build, finish, pivot, session, setup, store, CancellationToken, GraphStore};
use uqa_storage::catalog::graph_observations::GraphDefinitionKind;

#[test]
fn no_op_graph_mutations_observe_the_existence_decision() {
    for create in [false, true] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        if create {
            store(&a).create_graph("g").unwrap();
        }
        let mut read = store(&a).with_serializable_read(begin(&a), &CancellationToken::new());
        let other = begin(&b);
        if create {
            read.create_graph("g").unwrap();
        } else {
            read.drop_graph("g").unwrap();
        }
        pivot(&a, &other);
        if create {
            b.catalog.drop_named_graph("g").unwrap();
        } else {
            b.catalog.save_named_graph("g").unwrap();
        }
        finish(&a, &b, true);
    }
}

#[test]
fn graph_name_points_and_listings_track_presence_without_observing_unrelated_definitions() {
    for (list, selected, changed, create, conflict) in [
        (false, "missing", "missing", true, true),
        (false, "missing", "unrelated", true, false),
        (false, "missing", "missing", false, false),
        (false, "g", "g", false, true),
        (false, "g", "g", true, false),
        (true, "", "new", true, true),
        (true, "", "g", false, true),
    ] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        setup(&a);
        let read = store(&a).with_serializable_read(begin(&a), &CancellationToken::new());
        let other = begin(&b);
        if list {
            assert_eq!(read.graph_names().unwrap(), ["g", "other"]);
        } else {
            assert_eq!(read.has_graph(selected).unwrap(), selected == "g");
        }
        pivot(&a, &other);
        if create {
            b.catalog.save_named_graph(changed).unwrap();
        } else {
            b.catalog.drop_named_graph(changed).unwrap();
        }
        finish(&a, &b, conflict);
    }
}

#[test]
fn live_path_definitions_track_replacement_and_deletion_but_not_same_definition_rebuilds() {
    for mutation in ["replace", "drop", "same", "unrelated", "restore graph"] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        setup(&a);
        let sequence = vec!["knows".into()];
        let index = build(&a, &sequence);
        let snapshot = a.catalog.load_named_graph_snapshot("g").unwrap().unwrap();
        begin(&a);
        let other = begin(&b);
        assert_eq!(index.lookup(&sequence).unwrap().unwrap().len(), 1);
        pivot(&a, &other);
        match mutation {
            "replace" => b.catalog.save_path_index("g::p", "[[\"likes\"]]").unwrap(),
            "drop" => b.catalog.drop_path_index("g::p").unwrap(),
            "same" => b.catalog.save_path_index("g::p", "[[\"knows\"]]").unwrap(),
            "unrelated" => b.catalog.save_path_index("g::q", "[]").unwrap(),
            "restore graph" => b.catalog.replace_named_graph("g", &snapshot).unwrap(),
            _ => unreachable!(),
        }
        finish(&a, &b, !matches!(mutation, "same" | "unrelated"));
    }
}

#[test]
fn definition_write_observations_roll_back_with_their_storage_savepoint() {
    for read_before_rollback in [false, true] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        let read = store(&a).with_serializable_read(begin(&a), &CancellationToken::new());
        let other = begin(&b);
        if read_before_rollback {
            read.observe_definition(GraphDefinitionKind::PathIndex, Some("g::missing"))
                .unwrap();
        }
        pivot(&a, &other);
        let checkpoint = uqa_storage::StorageSavepointId::allocate();
        b.backend.savepoint(checkpoint).unwrap();
        b.catalog.save_path_index("g::missing", "[]").unwrap();
        b.backend.rollback_to_savepoint(checkpoint).unwrap();
        b.backend.release_savepoint(checkpoint).unwrap();
        assert!(b.catalog.load_path_indexes().unwrap().is_empty());
        if !read_before_rollback {
            read.observe_definition(GraphDefinitionKind::PathIndex, Some("g::missing"))
                .unwrap();
        }
        b.catalog.set_metadata("unrelated", "1").unwrap();
        finish(&a, &b, read_before_rollback);
        assert!(b.catalog.load_path_indexes().unwrap().is_empty());
    }
}

#[test]
fn retained_definition_reads_keep_the_original_participant_and_cancellation() {
    for cancelled in [false, true] {
        let database = session::Database::new();
        let a = database.session();
        let b = database.session();
        let context = begin(&a);
        let other = begin(&b);
        let cancellation = CancellationToken::new();
        let retained = a.backend.open_retained_read_session(&cancellation).unwrap();
        let read = store(&retained).with_serializable_read(context, &cancellation);
        if cancelled {
            cancellation.cancel();
            assert!(read.graph_names().is_err());
        } else {
            assert!(!read.has_graph("future").unwrap());
        }
        pivot(&a, &other);
        b.catalog.save_named_graph("future").unwrap();
        finish(&a, &b, !cancelled);
    }
}
