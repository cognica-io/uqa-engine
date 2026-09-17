//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, FunctionBinding, RelationPersistence, Statement},
    catalog::domain::StoredDomain,
    expr::EngineHook,
    plan::{QueryPlan, UnifiedPlan},
    routines::RoutineResolution,
    semantics::effects::QueryEffectCatalog,
    FunctionTypeResolver,
};

struct Catalog;
impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for Catalog {}
impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("effect analysis does not execute sequence functions")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("effect analysis does not execute sequence functions")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!("effect analysis does not execute sequence functions")
    }
}
impl QueryEffectCatalog for Catalog {
    fn registered_runtime_function_may_mutate_engine(&self, _: &str) -> bool {
        false
    }
    fn domain_by_oid(&self, _: u32) -> Option<StoredDomain> {
        None
    }
    fn sequence_persistence(&self, _: &str) -> Result<Option<RelationPersistence>, String> {
        Ok(None)
    }
    fn table_persistence(&self, _: &str) -> Result<Option<RelationPersistence>, String> {
        Ok(Some(RelationPersistence::Permanent))
    }
    fn view_plan(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
    fn lookup_prepared(&self, _: &str) -> Option<UnifiedPlan> {
        None
    }
}

fn effects(body: &str) -> [bool; 3] {
    let Statement::CreateFunction(definition) = crate::compile(&format!(
        "CREATE FUNCTION probe() RETURNS integer LANGUAGE plpgsql AS $$ {body} $$"
    ))
    .unwrap()
    .remove(0) else {
        panic!("function declaration")
    };
    let function = crate::plpgsql::parse_function(&definition).unwrap();
    let context = QueryEffectContext {
        catalog: &Catalog,
        optimizer_effects: |_| false,
        graph_effects: |_| Ok(false),
    };
    [
        MutabilityClassification::STATEMENT_TRANSACTION,
        MutabilityClassification::ENGINE_MUTATIONS,
        MutabilityClassification::DATABASE_WRITES,
    ]
    .map(|classification| {
        plpgsql_function_may_mutate_engine(
            &context,
            &function,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            classification,
        )
        .unwrap()
    })
}

#[test]
fn implicit_query_portals_require_transactions_without_becoming_database_writers() {
    for body in [
        "DECLARE rec record; BEGIN FOR rec IN SELECT v FROM items LOOP NULL; END LOOP; RETURN 1; END",
        "DECLARE rec record; BEGIN IF true THEN FOR rec IN SELECT 1 AS v LOOP EXIT; END LOOP; END IF; RETURN 1; END",
        "DECLARE c CURSOR FOR SELECT v FROM items; BEGIN FOR rec IN c LOOP NULL; END LOOP; RETURN 1; END",
        "DECLARE c refcursor; rec record; BEGIN OPEN c FOR SELECT v FROM items; FETCH c INTO rec; CLOSE c; RETURN 1; END",
    ] {
        assert_eq!(effects(body), [true, false, false], "{body}");
    }
    assert_eq!(effects("BEGIN PERFORM 1; RETURN 1; END"), [false; 3]);
}

#[test]
fn query_portals_preserve_mutation_effects_in_their_query_and_body() {
    for body in [
        "DECLARE rec record; BEGIN FOR rec IN INSERT INTO items VALUES (1) RETURNING v LOOP NULL; END LOOP; RETURN 1; END",
        "DECLARE rec record; BEGIN FOR rec IN SELECT v FROM items LOOP INSERT INTO output VALUES (rec.v); END LOOP; RETURN 1; END",
    ] {
        assert_eq!(effects(body), [true; 3], "{body}");
    }
}
