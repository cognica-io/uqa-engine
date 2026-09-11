//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, plan::UnifiedPlan, ColumnType, FunctionTypeResolver};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};

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
        panic!("dependency collection must not infer or evaluate function calls")
    }
}
impl RoutineResolution for NoRoutines {}

#[derive(Default)]
struct Catalog {
    visible: BTreeMap<String, (String, &'static str)>,
    tables: BTreeSet<String>,
    views: BTreeMap<String, QueryPlan>,
    descendants: BTreeMap<String, Vec<String>>,
    transition_reads: RefCell<VecDeque<BTreeSet<String>>>,
    sequence_error: Option<String>,
    events: RefCell<Vec<String>>,
}
impl Catalog {
    fn inputs(&self) -> PortalBindingContext<'_> {
        PortalBindingContext {
            catalog: self,
            routines: &NoRoutines,
            transitions: self,
        }
    }
    fn record(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }
}
impl PortalRelationCatalog for Catalog {
    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String> {
        self.record(format!("table:{name}"));
        Ok(self.tables.contains(name).then(|| name.to_string()))
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        self.record(format!("hierarchy:{table}"));
        Ok(if include_descendants {
            self.descendants
                .get(table)
                .cloned()
                .unwrap_or_else(|| vec![table.into()])
        } else {
            vec![table.into()]
        })
    }
    fn view_plan(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        self.record(format!("view:{name}"));
        Ok(self.views.get(name).cloned())
    }
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.record(format!("visible:{name}"));
        Ok(self.visible.get(name).cloned())
    }
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.record(format!("graph-label:{name}"));
        Ok(None)
    }
    fn try_resolve_sequence_reference(&self, name: &str) -> Result<Option<String>, String> {
        self.record(format!("sequence:{name}"));
        match &self.sequence_error {
            Some(error) => Err(error.clone()),
            None => Ok(None),
        }
    }
}
impl PortalTransitionRelations for Catalog {
    fn active_transition_relation_names(&self) -> BTreeSet<String> {
        self.record("transitions");
        self.transition_reads
            .borrow_mut()
            .pop_front()
            .unwrap_or_default()
    }
}
fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected a query plan");
    };
    *query
}

#[test]
fn sequential_ctes_bind_only_their_external_relation() {
    let catalog = Catalog {
        visible: BTreeMap::from([("base".into(), ("app.base".into(), "table"))]),
        ..Catalog::default()
    };
    let mut query = query("WITH first_rows AS (SELECT * FROM base), second_rows AS (SELECT * FROM first_rows) SELECT * FROM second_rows");
    relations::bind_session_portal_query_relations(&catalog.inputs(), &mut query, &BTreeSet::new())
        .unwrap();
    assert!(query.relations_bound);
    let events = catalog.events.borrow();
    let visible: Vec<_> = events
        .iter()
        .filter(|event| event.starts_with("visible:"))
        .map(String::as_str)
        .collect();
    assert_eq!(visible, ["visible:base"]);
}

#[test]
fn transition_relations_are_observed_at_each_reference() {
    let catalog = Catalog {
        transition_reads: RefCell::new(VecDeque::from([
            BTreeSet::from(["first_transition".into()]),
            BTreeSet::from(["second_transition".into()]),
        ])),
        ..Catalog::default()
    };
    let mut query = query("SELECT * FROM first_transition JOIN second_transition ON true");
    relations::bind_session_portal_query_relations(&catalog.inputs(), &mut query, &BTreeSet::new())
        .unwrap();
    assert_eq!(*catalog.events.borrow(), ["transitions", "transitions"]);
    assert!(catalog.transition_reads.borrow().is_empty());
}

#[test]
fn sequence_errors_precede_dependency_capture_and_preserve_context() {
    for provider_error in [None, Some("catalog unavailable".to_string())] {
        let catalog = Catalog {
            visible: BTreeMap::from([("base".into(), ("app.base".into(), "table"))]),
            sequence_error: provider_error.clone(),
            ..Catalog::default()
        };
        let mut query = query("SELECT nextval('missing_sequence') FROM base");
        let error = prepare_query(&catalog.inputs(), &mut query)
            .err()
            .expect("sequence binding must fail");
        match provider_error {
            Some(_) => assert!(
                matches!(error, SQLError::Internal(message) if message == "bind cursor sequence `missing_sequence` at DECLARE: catalog unavailable")
            ),
            None => assert!(
                matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "42P01" && message == "relation \"missing_sequence\" does not exist")
            ),
        }
        let events = catalog.events.borrow();
        assert!(events
            .iter()
            .any(|event| event == "sequence:missing_sequence"));
        assert!(!events.iter().any(|event| event.starts_with("table:")));
    }
}

#[test]
fn dynamic_graph_arguments_retain_all_graph_dependencies_without_evaluation() {
    let catalog = Catalog::default();
    let query = query("SELECT * FROM graph_pagerank($1)");
    let dependencies =
        dependencies::session_portal_table_dependencies(&catalog.inputs(), &query).unwrap();
    assert!(dependencies.graphs.is_none());
    assert!(dependencies.graph_catalog);
    assert_eq!(dependencies.tables, Some(BTreeSet::new()));
    assert!(catalog.events.borrow().is_empty());
}

#[test]
fn recursive_view_visitation_retains_table_descendants_and_terminates() {
    let catalog = Catalog {
        tables: BTreeSet::from(["app.base".into()]),
        views: BTreeMap::from([(
            "app.v".into(),
            query("SELECT * FROM app.base UNION ALL SELECT * FROM app.v"),
        )]),
        descendants: BTreeMap::from([(
            "app.base".into(),
            vec!["app.base".into(), "app.child".into()],
        )]),
        ..Catalog::default()
    };
    let dependencies = dependencies::session_portal_table_dependencies(
        &catalog.inputs(),
        &query("SELECT * FROM app.v"),
    )
    .unwrap();
    assert_eq!(
        dependencies.tables,
        Some(BTreeSet::from([
            RelationIdentity::new("app", "base"),
            RelationIdentity::new("app", "child"),
        ]))
    );
    assert_eq!(
        catalog
            .events
            .borrow()
            .iter()
            .filter(|event| *event == "view:app.v")
            .count(),
        1
    );
}

#[test]
fn operator_join_binding_preserves_left_resolution_before_right_kind_error() {
    let catalog = Catalog {
        visible: BTreeMap::from([
            ("left_source".into(), ("app.left_source".into(), "table")),
            ("right_source".into(), ("app.right_view".into(), "view")),
        ]),
        ..Catalog::default()
    };
    let mut relations = Some(crate::ast::OperatorJoinRelations {
        left: "left_source".into(),
        right: "right_source".into(),
    });
    let error =
        relations::bind_session_portal_function_relations(&catalog.inputs(), &mut relations)
            .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "42809" && message == "cursor table-function relation \"app.right_view\" is a view, not a table")
    );
    let relations = relations.unwrap();
    assert_eq!(relations.left, "app.left_source");
    assert_eq!(relations.right, "right_source");
    assert_eq!(
        *catalog.events.borrow(),
        ["visible:left_source", "visible:right_source"]
    );
}
