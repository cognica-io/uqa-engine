//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::graph_path_indexes::create_graph_with_edge;
use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use std::{collections::BTreeSet, path::Path, sync::Arc};
use uqa_core::Edge;
use uqa_graph::PathIndex;
use uqa_storage::PersistentStorageProvider;

const COMPANY: &str = "company";
const ARCHIVE: &str = "company::archive";
// This literal is the physical key written by older releases for either logical name.
const LEGACY_KEY: &str = "company::archive::idx";
const DEFINITION: &str = r#"[["edge"]]"#;

fn old_database(
    provider: Provider,
    path: &Path,
    graphs: &[&str],
    recorded_owner: Option<&str>,
) -> Arc<dyn PersistentStorageProvider> {
    let source = provider.open(path);
    for (i, graph) in graphs.iter().copied().enumerate() {
        let first = 2 * i as u64 + 1;
        create_graph_with_edge(&source, graph, first);
        source
            .add_graph_edge(Edge::new(200 + first, first + 1, first, "back"), graph)
            .unwrap();
    }
    let storage = Arc::clone(source.storage.provider.as_ref().unwrap());
    drop(source);
    let session = storage.open_session().unwrap();
    if let Some(graph) = recorded_owner {
        // Use the original materialization writer, including its saved owner and pair rows.
        PathIndex::build_persistent(
            Arc::clone(&session.catalog),
            session.backend,
            LEGACY_KEY,
            graph,
            &[vec!["edge".into()]],
        )
        .unwrap();
        if graph == ARCHIVE {
            session
                .catalog
                .save_path_index(LEGACY_KEY, DEFINITION)
                .unwrap();
            assert!(!session
                .catalog
                .path_index_data_is_current(LEGACY_KEY, DEFINITION)
                .unwrap());
        }
    } else {
        session
            .catalog
            .save_path_index(LEGACY_KEY, DEFINITION)
            .unwrap();
    }
    storage
}

fn check_owner(engine: &Engine, graph: &str, name: &str, label: &str, pair: (u64, u64)) {
    let neighbor = if graph == COMPANY {
        ("idx", ARCHIVE)
    } else {
        ("archive::idx", COMPANY)
    };
    engine
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    engine.list_graphs().unwrap();
    let index = engine
        .get_path_index(name, graph)
        .unwrap()
        .expect("original index");
    assert_eq!(
        index.lookup(&[label.into()]).unwrap(),
        Some(BTreeSet::from([pair]))
    );
    assert!(engine
        .get_path_index(neighbor.0, neighbor.1)
        .unwrap()
        .is_none());
    engine.sql("COMMIT", &[]).unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn legacy_path_indexes_reopen_restore_and_write_to_the_original_owner(#[case] provider: Provider) {
    for (graphs, owner, name, first, recorded) in [
        (&[COMPANY, ARCHIVE][..], ARCHIVE, "idx", 3, Some(ARCHIVE)),
        (
            &[COMPANY, ARCHIVE][..],
            COMPANY,
            "archive::idx",
            1,
            Some(COMPANY),
        ),
        (&[ARCHIVE][..], ARCHIVE, "idx", 1, None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("source");
        let repository = repository(&dir.path().join("repository"));
        let storage = old_database(provider, &database, graphs, recorded);
        let root = capture_provider(storage.as_ref(), &repository);
        drop(storage);
        check_owner(
            &provider.open(&database),
            owner,
            name,
            "edge",
            (first, first + 1),
        );

        let path = dir.path().join("restored");
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = Engine::new()
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                RestoreOptions::new(16 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        prepare(&mut job);
        let restored = job.publish().unwrap();
        drop(job);
        check_owner(&restored, owner, name, "edge", (first, first + 1));
        restored
            .build_path_index(name, owner, &[vec!["back".into()]])
            .unwrap();
        drop(restored);
        let installed = path.join("database");
        let reopened = provider.open(&installed);
        check_owner(&reopened, owner, name, "back", (first + 1, first));
        assert!(reopened.drop_path_index(name, owner).unwrap());
        drop(reopened);
        assert!(provider
            .open(&installed)
            .list_path_indexes()
            .unwrap()
            .is_empty());
        assert_eq!(control.memory().used(), 0);
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn legacy_path_indexes_reject_unresolvable_owners_before_publication(#[case] provider: Provider) {
    for (graphs, recorded, diagnostic) in [
        (&[COMPANY, ARCHIVE][..], None, "ambiguous path-index owner"),
        (
            &["unrelated"][..],
            None,
            "path index references a missing graph",
        ),
        (
            &[COMPANY, "unrelated"][..],
            Some("unrelated"),
            "recorded path-index owner disagrees with its key",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let repository = repository(&dir.path().join("repository"));
        let storage = old_database(provider, &dir.path().join("source"), graphs, recorded);
        let root = capture_provider(storage.as_ref(), &repository);
        let error = Engine::from_persistent_provider(storage)
            .err()
            .expect("invalid legacy database opened");
        assert!(
            error.to_string().contains(diagnostic),
            "{provider:?}: {error}"
        );

        let path = dir.path().join("restored");
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = Engine::new()
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                RestoreOptions::new(16 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        let error = loop {
            match job.advance(1024) {
                Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
                Err(error) => break error,
                Ok(phase) => panic!("invalid ownership reached {phase:?}"),
            }
        };
        assert!(
            error.to_string().contains(diagnostic),
            "{provider:?}: {error}"
        );
        assert_eq!(job.phase(), RestorePhase::Failed);
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
        assert!(!path.exists());
        assert_eq!(control.memory().used(), 0);
    }
}
