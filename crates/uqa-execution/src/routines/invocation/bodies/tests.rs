//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn function() -> SQLUserFunction {
    let uqa_sql::ast::Statement::CreateFunction(mut definition) = uqa_sql::compile(
        "CREATE FUNCTION inspect_body() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function")
    };
    definition.object_id = Some([4; 16]);
    SQLUserFunction::new(*definition, RoutineBody::Source)
}

fn body() -> CompiledFunctionBody {
    CompiledFunctionBody::SQL(vec![])
}

#[test]
fn static_inspection_does_not_initialize_the_execution_cache() {
    let cache = SessionRoutineBodies::default();
    let function = function();
    let first = cache.inspect(&function, |_| Ok(body())).unwrap();
    let second = cache.inspect(&function, |_| Ok(body())).unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    let executed = cache.body(&function, |_| Ok(body())).unwrap();
    assert!(!Arc::ptr_eq(&first, &executed));
    let retained = cache
        .inspect(&function, |_| {
            panic!("inspection retains executed type identities")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&executed, &retained));
}

#[test]
fn inspection_obeys_definition_revisions_without_retaining_failed_or_new_compilations() {
    let cache = SessionRoutineBodies::default();
    let function = function();
    let original = cache.body(&function, |_| Ok(body())).unwrap();
    let mut definition = function.def.clone();
    definition.catalog_revision = Some([9; 16]);
    let replacement = SQLUserFunction::new(definition, RoutineBody::Source);
    assert!(cache
        .inspect(&replacement, |_| Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: "missing type".into()
        }))
        .is_err());
    let temporary = cache.inspect(&replacement, |_| Ok(body())).unwrap();
    let executed = cache.body(&replacement, |_| Ok(body())).unwrap();
    assert!(!Arc::ptr_eq(&original, &executed));
    assert!(!Arc::ptr_eq(&temporary, &executed));
}
