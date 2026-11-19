//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn committed_sequence_images_preserve_positions_authority_and_owners_after_source_loss(
    #[case] completion: &str,
) {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    use uqa_execution::catalog::sequence::registry::SequenceCatalog;

    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE TABLE docs(id bigint); CREATE SEQUENCE saved START 41 INCREMENT 3 CACHE 4 OWNED BY docs.id; GRANT USAGE ON SEQUENCE saved TO reader; CREATE UNLOGGED SEQUENCE unlogged; CREATE TEMP SEQUENCE private_temp; SELECT nextval('saved')");
    let relation = RelationIdentity::new("public", "saved");
    let original = *engine.durable.sequences.read().get(&relation).unwrap();
    let original_security = engine.durable.sequences.read().security()[&relation].clone();
    let original_roles = engine.durable.roles.snapshot();
    sql(&engine, "BEGIN; ALTER SEQUENCE saved RESTART 99; REVOKE ALL ON SEQUENCE saved FROM reader; CREATE SEQUENCE private_sequence; SAVEPOINT nested; DROP SEQUENCE unlogged; ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(1 << 20);
    let selected = capture(&engine, &control);
    sql(&engine, completion);
    sql(&engine, "DROP TABLE docs; CREATE SEQUENCE saved START 1");
    drop(engine);
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    drop(selected);
    let token = CancellationToken::new();
    let mut metadata = Vec::new();
    let mut authority = Vec::new();
    sequences
        .get()
        .unwrap()
        .image_reader(&token)
        .unwrap()
        .write_to(&mut metadata)
        .unwrap();
    sequences
        .get()
        .unwrap()
        .authority_image_reader(&token)
        .unwrap()
        .write_to(&mut authority)
        .unwrap();
    drop(sequences);
    assert_eq!(control.memory().used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    let restored = SequenceCatalog::read_images(
        &mut metadata.as_slice(),
        &mut authority.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored.get(&relation), Some(&original));
    assert!(restored.state(&relation).unwrap().owner.is_some());
    assert_eq!(restored.security()[&relation], original_security);
    restored.security()[&relation]
        .validate(&original_roles)
        .unwrap();
    assert!(!restored.contains_key(&RelationIdentity::new("public", "private_sequence")));
    assert_eq!(
        restored.persistence(&RelationIdentity::new("public", "unlogged")),
        Some(uqa_sql::ast::RelationPersistence::Unlogged)
    );
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_sequence_sources_preserve_joint_durable_records_after_unread_retirement(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1); CREATE SEQUENCE saved START WITH 41 INCREMENT BY 3 CACHE 4; GRANT USAGE ON SEQUENCE saved TO reader; CREATE UNLOGGED SEQUENCE unlogged; CREATE TEMP SEQUENCE temporary_sequence; SELECT nextval('saved')");
    let relation = RelationIdentity::new("public", "saved");
    let original_state = *engine.durable.sequences.read().state(&relation).unwrap();
    let original_id = *engine
        .durable
        .sequences
        .read()
        .object_id(&relation)
        .unwrap();
    let original_oid = *engine
        .durable
        .sequences
        .read()
        .catalog_oids()
        .get(&original_id)
        .unwrap();
    let original_acl = engine.durable.sequences.read().security()[&relation].clone();
    sql(&engine, "BEGIN; UPDATE docs SET id=2; ALTER SEQUENCE saved RESTART WITH 99; REVOKE ALL ON SEQUENCE saved FROM reader; CREATE SEQUENCE private_sequence; SAVEPOINT nested; ALTER SEQUENCE unlogged RENAME TO changed; ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    assert!(selected
        .sequences
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    assert!(selected
        .sequences
        .budget()
        .shares_allowance(control.memory()));
    sql(&engine, completion);
    sql(&engine, "DROP SEQUENCE saved; CREATE SEQUENCE saved START 1; DROP TABLE docs; CREATE TABLE docs(replacement boolean)");
    drop(engine);
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    assert!(sequences.budget().shares_allowance(control.memory()));
    let captured = sequences.get().unwrap();
    let rows = captured.iter().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].relation, &relation);
    assert_eq!(rows[0].object_id, original_id);
    assert_eq!(rows[0].catalog_oid, Some(original_oid));
    assert_eq!(rows[0].state, original_state);
    assert_eq!(rows[0].security, &original_acl);
    assert_eq!(
        rows[1].relation,
        &RelationIdentity::new("public", "unlogged")
    );
    assert_eq!(
        rows[1].persistence,
        uqa_sql::ast::RelationPersistence::Unlogged
    );
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "id")
            .unwrap(),
        Some(Value::Int(1))
    );
    drop(selected);
    assert!(control.memory().used() > 0);
    control.cancellation().cancel();
    assert!(sequences.get().is_err());
    drop(sequences);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn transaction_sequence_source_excludes_temporary_authority_during_a_private_callback() {
    let engine = Arc::new(Engine::new());
    sql(&engine, "CREATE SEQUENCE saved START 41; CREATE TEMP SEQUENCE temporary_sequence; GRANT USAGE ON SEQUENCE temporary_sequence TO PUBLIC");
    let temp = engine
        .durable
        .sequences
        .read()
        .persistences()
        .find_map(|(name, persistence)| {
            (*persistence == uqa_sql::ast::RelationPersistence::Temporary).then(|| name.clone())
        })
        .unwrap();
    let mut catalog = engine.durable.sequences.write();
    let mut security = catalog.security()[&temp].clone();
    let acl = security.acl.as_mut().unwrap();
    let entry = acl[0].clone();
    acl.resize(16_384, entry);
    catalog.replace_security(&temp, security).unwrap();
    drop(catalog);
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let writer = engine.clone();
    let worker = std::thread::spawn(move || {
        writer.transaction(|engine| {
        sql(engine, "ALTER SEQUENCE saved RESTART 99; CREATE SEQUENCE private_sequence; SAVEPOINT nested");
        entered.send(()).unwrap();
        proceed.recv().unwrap();
        Ok(())
    })
    });
    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let control = StorageReadControl::with_limit(8192);
    let selected = capture(&engine, &control);
    let saved = selected.sequences.captured(&selected.cut).unwrap();
    assert_eq!(saved.get().unwrap().len(), 1);
    assert_eq!(
        saved.get().unwrap().iter().next().unwrap().state.current,
        41
    );
    assert!(!saved.get().unwrap().iter().next().unwrap().state.called);
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    drop((engine, selected));
    assert_eq!(
        saved.get().unwrap().iter().next().unwrap().relation,
        &RelationIdentity::new("public", "saved")
    );
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}
