//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_event_roots_preserve_original_definitions_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        sql(&engine,"CREATE TABLE items(id integer); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$");
        for index in 0..count {
            sql(&engine,&format!("CREATE TRIGGER saved_{index:03} BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION fire('original'); CREATE RULE saved_{index:03} AS ON DELETE TO items DO NOTHING"));
        }
        let relation = RelationIdentity::new("public", "items");
        let original_triggers = engine.durable.triggers.snapshot();
        let original_rules = engine.durable.rules.snapshot();
        let trigger_owner = original_triggers.identity();
        let rule_owner = original_rules.identity();
        let expected_triggers = serde_json::to_value(&original_triggers[&relation]).unwrap();
        let expected_rules = serde_json::to_value(&original_rules[&relation]).unwrap();
        drop((original_triggers, original_rules));
        sql(&engine,"BEGIN; ALTER TABLE items DISABLE TRIGGER saved_000; CREATE OR REPLACE RULE saved_000 AS ON DELETE TO items DO INSTEAD NOTHING; SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some((
                selected.catalogs.triggers.captured(&selected.cut).unwrap(),
                selected.catalogs.rules.captured(&selected.cut).unwrap(),
            ));
        });
        assert!(
            allocations.bytes_total < 32 << 10 && allocations.count_total < 128,
            "count={count}: {allocations:?}"
        );
        let (triggers, rules) = saved.unwrap();
        sql(&engine,"DROP TRIGGER saved_000 ON items; DROP RULE saved_000 ON items; ROLLBACK TO changed; COMMIT; DROP TABLE items; CREATE TABLE items(replacement text)");
        drop(engine);
        assert!(!trigger_owner.is_alive());
        assert!(!rule_owner.is_alive());
        assert_eq!(triggers.get().unwrap().len(), 1);
        assert_eq!(rules.get().unwrap().len(), 1);
        assert_eq!(
            serde_json::to_value(triggers.get().unwrap().get(&relation).unwrap()).unwrap(),
            expected_triggers
        );
        assert_eq!(
            serde_json::to_value(rules.get().unwrap().get(&relation).unwrap()).unwrap(),
            expected_rules
        );
        drop(selected);
        control.cancellation().cancel();
        assert!(triggers.get().is_err());
        assert!(rules.get().is_err());
        drop((triggers, rules));
        assert_eq!(control.memory().used(), 0);
    }
}
