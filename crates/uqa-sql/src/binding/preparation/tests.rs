//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, FunctionTypeResolver, RelationIdentity};
use std::collections::{BTreeMap, BTreeSet};

struct NoRoutines;
impl FunctionTypeResolver for NoRoutines {
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
impl RoutineResolution for NoRoutines {}

#[test]
fn analyzer_table_functions_infer_text_parameters_in_queries_and_insert_sources() {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE diagnostic_snapshot (analysis JSONB)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "diagnostic_snapshot"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        resolution: crate::binding::fixture::resolution(
            vec!["public".into()],
            "pg_temp_fixture".into(),
        ),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: BTreeSet::new(),
        scalar_subqueries: &[],
    };
    for (sql, count) in [
        ("SELECT * FROM analyze_text($1, $2)", 2),
        (
            "INSERT INTO diagnostic_snapshot SELECT analysis FROM analyze_text($1, $2)",
            2,
        ),
        ("SELECT * FROM create_analyzer($1, $2)", 2),
        ("SELECT * FROM drop_analyzer($1)", 1),
        ("SELECT * FROM set_table_analyzer($1, $2, $3)", 3),
        ("SELECT * FROM set_table_analyzer($1, $2, $3, $4)", 4),
        ("SELECT * FROM fts_index_stats($1)", 1),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let types =
            infer_prepared_parameter_types(&NoRoutines, &plan, &vec![None; count], &context)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(types, vec![Some(ColumnType::Text); count], "{sql}");
    }
}
