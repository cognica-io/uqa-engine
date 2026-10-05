//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    analysis::{AnalysisCatalog, TableDefinition, ViewDefinition},
    resolution::RelationNameResolution,
};
use crate::prepared::dependencies::PreparedDependencyRevision;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Catalog {
    revision: Mutex<Option<u64>>,
    requests: Mutex<Vec<PreparedAnalysisDependencies>>,
}

impl AnalysisCatalog for Catalog {
    fn prepared_dependency_snapshot(
        &self,
        dependencies: &PreparedAnalysisDependencies,
    ) -> Result<Option<PreparedDependencySnapshot>, SQLError> {
        self.requests.lock().unwrap().push(dependencies.clone());
        let revision = *self.revision.lock().unwrap();
        Ok(Some(PreparedDependencySnapshot {
            global_catalog: None,
            relations: dependencies
                .relations
                .iter()
                .map(|oid| (*oid, revision.map(PreparedDependencyRevision::new)))
                .collect(),
            routines: dependencies
                .routines
                .iter()
                .map(|id| (*id, revision.map(PreparedDependencyRevision::new)))
                .collect(),
        }))
    }
    fn effective_search_path(
        &self,
        resolution: &RelationNameResolution,
    ) -> Result<Option<EffectiveSearchPath>, SQLError> {
        NoRoutines.effective_search_path(resolution)
    }
    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        NoRoutines.table_resolved(resolution, name)
    }
    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        NoRoutines.table_name_resolved(resolution, name)
    }
    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        NoRoutines.view_resolved(resolution, name)
    }
    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        NoRoutines.foreign_table_resolved(resolution, name)
    }
    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        NoRoutines.sequence_exists(resolution, name)
    }
    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        NoRoutines.virtual_relation_schema(resolution, name)
    }
    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<crate::routines::SQLUserFunction>>>, SQLError> {
        NoRoutines.sql_functions(resolution, name)
    }
}

struct Aliases;
impl crate::schema::dependencies::oid_alias::OidAliasInput for Aliases {
    fn resolve_oid_alias_input(&self, _: &ColumnType, name: &str) -> Result<Option<i64>, SQLError> {
        Ok((name == "input_relation").then_some(41))
    }
}

fn context(scopes: &Scopes) -> PreparedDefinitionContext<'_> {
    PreparedDefinitionContext {
        types: &NoRoutines,
        routines: &NoRoutines,
        scopes,
        aliases: &Aliases,
    }
}

#[test]
fn prepared_definition_keeps_converted_oid_and_matching_catalog_revisions_together() {
    let catalog = Arc::new(Catalog {
        revision: Mutex::new(Some(7)),
        requests: Mutex::default(),
    });
    let scopes = Scopes {
        catalog: Some(catalog.clone()),
        ..Scopes::default()
    };
    let definition = analyze_definition(
        &context(&scopes),
        UnifiedPlan::lower(
            crate::compile("SELECT 'input_relation'::regclass")
                .unwrap()
                .remove(0),
        ),
        &[],
    )
    .unwrap();
    assert_eq!(
        definition.dependencies.relations,
        [41].into_iter().collect()
    );
    assert_eq!(
        catalog.requests.lock().unwrap().as_slice(),
        std::slice::from_ref(&definition.dependencies)
    );
    assert_eq!(
        definition.dependency_snapshot.as_ref().unwrap().relations,
        BTreeMap::from([(41, Some(PreparedDependencyRevision::new(7_u64)))])
    );
    let mut values = vec![];
    definition
        .logical_plan
        .visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |expression| {
                if let crate::ScalarExpr::TypedLiteral {
                    value,
                    bound_type: Some(ColumnType::Regclass),
                    ..
                } = expression
                {
                    values.push(value.clone());
                }
            });
        });
    assert_eq!(values, [uqa_core::Value::Int(41)]);
    let current = || {
        analysis_is_current(
            &context(&scopes),
            definition.effective_search_path.as_ref(),
            &definition.dependencies,
            definition.dependency_snapshot.as_ref(),
        )
        .unwrap()
    };
    assert!(current());
    *catalog.revision.lock().unwrap() = Some(8);
    assert!(!current());
    *catalog.revision.lock().unwrap() = None;
    assert!(!current());
    *catalog.revision.lock().unwrap() = Some(7);
    assert!(current());
    assert_eq!(scopes.captures.get(), 6);
}

#[test]
fn dependency_freshness_distinguishes_unsupported_catalogs_and_restored_search_paths() {
    let retained = PreparedDependencySnapshot {
        global_catalog: None,
        relations: BTreeMap::from([(41, None)]),
        routines: BTreeMap::new(),
    };
    let dependencies = PreparedAnalysisDependencies {
        relations: [41].into_iter().collect(),
        routines: std::collections::BTreeSet::default(),
    };
    let mut scopes = Scopes::default();
    assert!(!analysis_is_current(&context(&scopes), None, &dependencies, Some(&retained)).unwrap());
    assert!(analysis_is_current(&context(&scopes), None, &dependencies, None).unwrap());
    assert_eq!(scopes.captures.get(), 1);
    let retained_path = effective_search_path(&context(&scopes)).unwrap().unwrap();
    scopes.search_path = Some(vec!["other_schema".into()]);
    assert!(
        !analysis_is_current(&context(&scopes), Some(&retained_path), &dependencies, None).unwrap()
    );
    scopes.search_path = Some(vec!["public".into()]);
    assert!(
        analysis_is_current(&context(&scopes), Some(&retained_path), &dependencies, None).unwrap()
    );
}
