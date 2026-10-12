//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field selection retains declaration metadata rather than combining unrelated arguments.

use super::*;
use crate::catalog::composite_type::{StoredComposite, StoredCompositeAttribute};
use crate::expr::composites::{CompositeTypeCatalog, CompositeTypeDescriptor};
use std::sync::Arc;

struct Types;

fn pair(name: &str) -> StoredComposite {
    let oid = if name == "pair" { 40_001 } else { 40_011 };
    StoredComposite {
        object_id: [1; 16],
        oid,
        relation_oid: oid + 1,
        array_oid: oid + 2,
        array_name: format!("_{name}"),
        identity: RelationIdentity::new("public", name),
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        usage_acl: None,
        attributes: ["a", "b"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| StoredCompositeAttribute {
                name: name.into(),
                ty: if oid == 40_001 {
                    ColumnType::Text
                } else {
                    domain("d_posix", ColumnType::Text)
                },
                collation: None,
                collation_oid: Some(if index == 0 { 951 } else { 950 }),
                number: i16::try_from(index + 1).unwrap(),
                dropped: false,
            })
            .collect(),
    }
}

fn domain(name: &str, base: ColumnType) -> ColumnType {
    let text = name == "d_posix";
    ColumnType::Domain {
        schema: "public".into(),
        name: name.into(),
        oid: if text { 40_021 } else { 40_031 },
        array_oid: None,
        collation_oid: Some(if text { 951 } else { 0 }),
        base: Box::new(base),
    }
}

impl FunctionTypeResolver for Types {
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

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        let (name, array) = name
            .strip_suffix("[]")
            .map_or((name, false), |name| (name, true));
        let name = name.strip_prefix("public.").unwrap_or(name);
        let ty = match name {
            "pair" | "domain_fields" => pair(name).column_type(),
            "d_posix" => domain(name, ColumnType::Text),
            "domain_pair" => domain(name, pair("pair").column_type()),
            _ => return Ok(None),
        };
        Ok(Some(if array {
            ColumnType::Array(Box::new(ty))
        } else {
            ty
        }))
    }

    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
}

impl RoutineResolution for Types {}

impl CompositeTypeCatalog for Types {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(match oid {
            40_001 => Some(Arc::new(pair("pair").descriptor())),
            40_011 => Some(Arc::new(pair("domain_fields").descriptor())),
            _ => None,
        })
    }
}

#[test]
fn declared_field_collations_and_diagnostic_order_match_postgresql() {
    super::column_collations::assert_collation_references(
        &Types,
        &[(include_str!("declared_field_collations.json"), 22)],
    );
}

#[test]
fn anonymous_field_collations_and_descriptor_errors_match_postgresql() {
    super::column_collations::assert_collation_references(
        &Types,
        &[
            (include_str!("anonymous_field_collations.json"), 50),
            (include_str!("anonymous_field_boundaries.json"), 15),
        ],
    );
}

#[test]
fn anonymous_fields_distinguish_parameter_assignment_from_casts() {
    for (expression, declared, collation) in [
        ("$1::text", None, 100),
        ("$1::d_posix", None, 951),
        ("$1::varchar(2)", None, 0),
        ("$1::d_posix", Some(ColumnType::Text), 0),
    ] {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            crate::compile(&format!("SELECT (ROW({expression})).f1"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let binding = crate::binding::fixture::empty_binding_context();
        let mut preparation = Preparation {
            routines: &Types,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[declared]),
            schema_expression: None,
        };
        let output = preparation.query(&query, None).unwrap();
        assert_eq!(output.column_collation(0), Some(collation), "{expression}");
    }
}

#[test]
fn anonymous_field_metadata_observes_each_call_once_per_analysis() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Calls(AtomicUsize);
    impl FunctionTypeResolver for Calls {
        fn resolve_function_type(
            &self,
            name: &str,
            _: Option<&FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok((name == "application.identity").then(|| {
                self.0.fetch_add(1, Ordering::Relaxed);
                ColumnType::Text
            }))
        }
    }
    impl RoutineResolution for Calls {}
    for depth in [1, 17] {
        let mut expression = "NULL::text COLLATE \"C\"".to_owned();
        for _ in 0..depth {
            expression = format!("application.identity({expression})");
        }
        for (sql, collation) in [
            (
                format!("SELECT (ROW({expression})).f1 AS value ORDER BY value"),
                0,
            ),
            (
                format!("SELECT (((ROW({expression})::record)::record)::record).f1 AS value ORDER BY value"),
                0,
            ),
            (
                format!(
                    "SELECT (r).f1 AS value FROM (SELECT ROW({expression}) AS r) q ORDER BY value"
                ),
                950,
            ),
        ] {
            let UnifiedPlan::Query(query) =
                UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let calls = Calls(AtomicUsize::new(0));
            let binding = crate::binding::fixture::empty_binding_context();
            if let crate::plan::RelationalPlan::QueryBlock(block) = &query.root {
                if block.from.is_none() {
                    let ty = crate::scalar_type_with_resolver(&block.projections[0].expr, &RowSchema::default(), &[], &calls).unwrap();
                    assert_eq!(ty, Some(ColumnType::Text));
                    assert_eq!(calls.0.load(Ordering::Relaxed), depth, "pure type inference: {sql}");
                }
            }
            for strict in [false, true] {
                calls.0.store(0, Ordering::Relaxed);
                let output = if strict {
                    crate::binding::analyze_query_plan_schema(&calls, &query, &[], &binding, None)
                } else {
                    crate::binding::bind_query_plan_schema(&calls, &query, &[], &binding, None)
                }
                .unwrap();
                assert_eq!(output.column_collation(0), Some(collation), "{sql}");
                assert_eq!(
                    calls.0.load(Ordering::Relaxed),
                    depth * if strict { 2 } else { 1 },
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn named_field_metadata_reuses_each_observed_call_argument() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Calls(AtomicUsize);
    impl FunctionTypeResolver for Calls {
        fn resolve_function_type(
            &self,
            name: &str,
            _: Option<&FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok((name == "application.identity").then(|| {
                self.0.fetch_add(1, Ordering::Relaxed);
                pair("pair").column_type()
            }))
        }
        fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
            Types.resolve_type_name(name)
        }
        fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
            Some(&Types)
        }
    }
    impl RoutineResolution for Calls {}
    for depth in [1, 17] {
        let mut expression = "NULL::pair".to_owned();
        for _ in 0..depth {
            expression = format!("application.identity({expression})");
        }
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            crate::compile(&format!("SELECT ({expression}).a AS value ORDER BY value"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let calls = Calls(AtomicUsize::new(0));
        let binding = crate::binding::fixture::empty_binding_context();
        for strict in [false, true] {
            calls.0.store(0, Ordering::Relaxed);
            let output = if strict {
                crate::binding::analyze_query_plan_schema(&calls, &query, &[], &binding, None)
            } else {
                crate::binding::bind_query_plan_schema(&calls, &query, &[], &binding, None)
            }
            .unwrap();
            assert_eq!(output.column_type(0), Some(&ColumnType::Text));
            assert_eq!(output.column_collation(0), Some(951));
            assert_eq!(
                calls.0.load(Ordering::Relaxed),
                depth * if strict { 2 } else { 1 }
            );
        }
    }
}

#[test]
fn declared_descriptors_keep_selected_legacy_and_type_default_collations() {
    let mut definition = pair("pair");
    definition.attributes[0].collation_oid = None;
    definition.attributes[0].collation = Some("POSIX".into());
    definition.attributes[1].collation = Some("POSIX".into());
    let selected = definition.descriptor();
    assert_eq!(selected.attributes[0].collation_oid(), 951);
    assert_eq!(selected.attributes[1].collation_oid(), 950);
    let encoded = serde_json::to_value(&selected).unwrap();
    assert_eq!(
        serde_json::from_value::<CompositeTypeDescriptor>(encoded.clone()).unwrap(),
        selected
    );
    let mut legacy = encoded;
    legacy["attributes"][0]
        .as_object_mut()
        .unwrap()
        .remove("collation_oid");
    let legacy: CompositeTypeDescriptor = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.attributes[0].collation_oid(), 100);
}
