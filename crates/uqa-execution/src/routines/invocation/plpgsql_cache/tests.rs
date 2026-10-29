//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
use super::*;
use uqa_core::memory::ProductionControl;
use uqa_sql::ast::Statement;
use uqa_sql::routines::RoutineBody;
fn routine() -> SQLUserFunction {
    let Statement::CreateFunction(mut def)=uqa_sql::compile("CREATE FUNCTION first_use(v integer) RETURNS text LANGUAGE plpgsql AS $$BEGIN RETURN v::text; END$$").unwrap().remove(0) else {panic!("routine")};
    def.object_id = Some([2; 16]);
    SQLUserFunction::new(*def, RoutineBody::Source)
}
fn compile(def: &CreateFunction) -> Result<CompiledFunctionBody, SQLError> {
    uqa_sql::plpgsql::parse_function(def).map(CompiledFunctionBody::PLpgSQL)
}
fn parsed(body: &CompiledFunctionBody) -> &PLpgSQLFunction {
    let CompiledFunctionBody::PLpgSQL(body) = body else {
        panic!("procedural body")
    };
    body
}
#[test]
fn validator_cache_is_shared_only_by_the_same_concrete_signature_and_trigger_relation() {
    let function = routine();
    let cache = SessionRoutineBodies::default();
    cache
        .retain(&function, compile(&function.def).unwrap())
        .unwrap();
    let integer = cache
        .plpgsql_body(
            &function,
            &function.def,
            None,
            &ProductionControl::uncontrolled(),
            |_| panic!("validator body"),
        )
        .unwrap();
    let prep = cache
        .plpgsql_preparations(
            &function.def,
            parsed(&integer),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
    assert!(Arc::ptr_eq(
        &prep,
        &cache
            .plpgsql_preparations(
                &function.def,
                &parsed(&integer).clone(),
                &ProductionControl::uncontrolled()
            )
            .unwrap()
    ));
    let mut text = function.def.clone();
    text.params[0].type_name = "text".into();
    let text_body = cache
        .plpgsql_body(
            &function,
            &text,
            None,
            &ProductionControl::uncontrolled(),
            compile,
        )
        .unwrap();
    assert!(!Arc::ptr_eq(
        &prep,
        &cache
            .plpgsql_preparations(
                &text,
                parsed(&text_body),
                &ProductionControl::uncontrolled()
            )
            .unwrap()
    ));
    let first = cache
        .plpgsql_body(
            &function,
            &function.def,
            Some(10),
            &ProductionControl::uncontrolled(),
            compile,
        )
        .unwrap();
    let second = cache
        .plpgsql_body(
            &function,
            &function.def,
            Some(11),
            &ProductionControl::uncontrolled(),
            compile,
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    let again = cache
        .plpgsql_body(
            &function,
            &function.def,
            Some(10),
            &ProductionControl::uncontrolled(),
            |_| panic!("same relation"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&first, &again));
}
#[test]
fn replacement_and_rollback_evict_dead_versions_without_resurrecting_their_preparations() {
    let function = routine();
    let cache = SessionRoutineBodies::default();
    let original = cache
        .plpgsql_body(
            &function,
            &function.def,
            None,
            &ProductionControl::uncontrolled(),
            compile,
        )
        .unwrap();
    let preparation = cache
        .plpgsql_preparations(
            &function.def,
            parsed(&original),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
    let weak = Arc::downgrade(&preparation);
    drop(preparation);
    let mut definition = function.def.clone();
    definition.catalog_revision = Some([9; 16]);
    let replacement = SQLUserFunction::new(definition, RoutineBody::Source);
    cache
        .retain(&replacement, compile(&replacement.def).unwrap())
        .unwrap();
    assert!(weak.upgrade().is_none());
    let rolled_back = cache
        .plpgsql_body(
            &function,
            &function.def,
            None,
            &ProductionControl::uncontrolled(),
            compile,
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&original, &rolled_back));
    assert!(!parsed(&original)
        .compilation
        .same(&parsed(&rolled_back).compilation));
}
#[test]
fn anonymous_activation_preparations_are_not_retained_by_the_session() {
    let function = routine();
    let cache = SessionRoutineBodies::default();
    let body = compile(&function.def).unwrap();
    let preparation = cache
        .plpgsql_preparations(
            &function.def,
            parsed(&body),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
    let weak = Arc::downgrade(&preparation);
    drop(preparation);
    assert!(weak.upgrade().is_none());
    assert!(cache.compiled.lock().is_empty());
}

#[test]
fn specialization_admits_keys_nodes_and_observers_and_reuses_their_owners() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let function = routine();
    let cache = SessionRoutineBodies::default();
    cache
        .retain(&function, compile(&function.def).unwrap())
        .unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let occupied = budget
        .reserve(budget.available() - OwnedMap::<usize, PreparedBody>::entry_bytes() + 1)
        .unwrap();
    let rejected = cache.plpgsql_body(&function, &function.def, None, &control, |_| {
        panic!("validator is already retained")
    });
    let error = match rejected {
        Err(error) => error,
        Ok(_) => panic!("specialization must be rejected before publication"),
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.bytes());
    assert!(cache.compiled.lock()[&[2; 16]].plpgsql.is_empty());
    drop(occupied);
    let body = cache
        .plpgsql_body(&function, &function.def, None, &control, |_| {
            panic!("validator is already retained")
        })
        .unwrap();
    let preparation = cache
        .plpgsql_preparations(&function.def, parsed(&body), &control)
        .unwrap();
    let retained = budget.used();
    assert!(retained > 0);
    let repeated = cache
        .plpgsql_body(&function, &function.def, None, &control, |_| {
            panic!("matching specialization")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&body, &repeated));
    assert_eq!(budget.used(), retained);
    drop(cache);
    assert!(budget.used() > 0 && budget.used() < retained);
    drop(preparation);
    assert_eq!(budget.used(), 0);
}
