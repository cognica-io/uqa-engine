//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_event_sources_keep_original_bindings_and_exclude_temporary_bodies(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER saved BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION fire('original'); CREATE RULE changed AS ON UPDATE TO docs DO NOTHING; CREATE TEMP TABLE temporary_table(id integer); CREATE TRIGGER temporary_trigger BEFORE INSERT ON temporary_table FOR EACH ROW EXECUTE FUNCTION fire('temporary'); CREATE RULE temporary_rule AS ON UPDATE TO temporary_table DO NOTHING; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote_rows(id integer) SERVER remote; CREATE FOREIGN TABLE pg_temp.temporary_foreign(payload text) SERVER remote");
    let relation = RelationIdentity::new("public", "docs");
    let trigger = engine.durable.triggers.read()[&relation]["saved"].clone();
    let rule = engine.durable.rules.read()[&relation]["changed"].clone();
    engine
        .durable
        .triggers
        .write()
        .rewrite(|relation, _, entry| {
            if !uqa_sql::catalog::temporary_namespace::is_temporary_schema_name(&relation.schema) {
                return Ok(None);
            }
            let mut entry = entry.clone();
            entry.definition.arguments.push("t".repeat(1 << 20));
            Ok(Some(entry))
        })
        .unwrap();
    engine
        .durable
        .rules
        .write()
        .rewrite(|relation, _, entry| {
            if !uqa_sql::catalog::temporary_namespace::is_temporary_schema_name(&relation.schema) {
                return Ok(None);
            }
            let mut entry = entry.clone();
            entry.definition.name = "t".repeat(1 << 20);
            Ok(Some(entry))
        })
        .unwrap();
    for definition in engine.durable.foreign_tables.write().values_mut() {
        if definition.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            definition
                .options
                .insert("large".into(), "t".repeat(1 << 20));
        }
    }
    sql(&engine, "BEGIN; UPDATE docs SET id=2; ALTER TABLE docs DISABLE TRIGGER saved; CREATE OR REPLACE RULE changed AS ON UPDATE TO docs DO INSTEAD NOTHING; SAVEPOINT nested; DROP TRIGGER saved ON docs; ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    assert!(selected
        .catalogs
        .triggers
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    assert!(selected
        .catalogs
        .rules
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    sql(&engine, completion);
    sql(
        &engine,
        "DROP TABLE docs; CREATE TABLE docs(replacement boolean)",
    );
    drop(engine);
    let triggers = selected.catalogs.triggers.captured(&selected.cut).unwrap();
    assert_eq!(triggers.get().unwrap().len(), 1);
    assert_eq!(triggers.get().unwrap().iter().next().unwrap().0, &relation);
    assert_eq!(
        serde_json::to_value(&triggers.get().unwrap().get(&relation).unwrap()["saved"]).unwrap(),
        serde_json::to_value(&trigger).unwrap()
    );
    let rules = selected.catalogs.rules.captured(&selected.cut).unwrap();
    assert_eq!(rules.get().unwrap().len(), 1);
    let saved = &rules.get().unwrap().get(&relation).unwrap()["changed"];
    assert_eq!(saved.catalog_oid, rule.catalog_oid);
    assert_eq!(saved.enabled, rule.enabled);
    assert_eq!(
        serde_json::to_value(&saved.definition).unwrap(),
        serde_json::to_value(&rule.definition).unwrap()
    );
    let foreign = selected.foreign.captured(&selected.cut).unwrap();
    assert_eq!(foreign.get().unwrap().len(), 1);
    assert_eq!(
        foreign.get().unwrap()[0].relation,
        RelationIdentity::new("public", "remote_rows")
    );
    drop((triggers, rules, foreign, selected));
    assert_eq!(control.memory().used(), 0);
}
