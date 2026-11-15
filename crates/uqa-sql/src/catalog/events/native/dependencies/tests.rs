//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{EventEnableMode, Statement},
    catalog::{
        events::{RuleColumnDependency, RuleDependencies},
        resolution::RelationLookupMode,
    },
    semantics::privileges::context::{PrivilegeRelation, PrivilegeRelationKind},
};
use uqa_core::{
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

struct Catalog;
impl PrivilegeCatalog for Catalog {
    fn relation(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        panic!("native dependency analysis must use admitted metadata")
    }
    fn relation_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<PrivilegeRelation>>, SQLError> {
        assert_eq!(resolution.lookup_mode, RelationLookupMode::Bound);
        PrivilegeRelation::from_names(
            name,
            ["id", "a", "b", "flag"],
            PrivilegeRelationKind::Table,
            control,
        )
        .map(Some)
    }
    fn has_select_privilege(
        &self,
        _: &RelationNameResolution,
        _: &PrivilegeRelation,
        _: Option<&str>,
        _: &RoleReference,
    ) -> Result<bool, SQLError> {
        panic!("dependency projection does not authorize an execution")
    }
}

fn namespace() -> RelationNameResolution {
    RelationNameResolution {
        search_path: Vec::new(),
        temporary_schema: String::new(),
        temporary_namespace_allocated: false,
        current_user: "reader".into(),
        lookup_mode: RelationLookupMode::Bound,
    }
}

fn rule(
    action: &str,
    condition: Option<&str>,
    relations: &[&str],
    columns: &[(&str, &str)],
) -> StoredRule {
    let condition = condition.map_or_else(String::new, |condition| format!(" WHERE {condition}"));
    let Statement::CreateRule(definition) = crate::compile(&format!(
        "CREATE RULE saved AS ON UPDATE TO public.items{condition} DO {action}"
    ))
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    let mut dependencies = RuleDependencies::default();
    for relation in relations {
        dependencies
            .relations
            .insert(RelationIdentity::from_legacy_name(relation).unwrap());
    }
    for (relation, column) in columns {
        dependencies.columns.insert(RuleColumnDependency {
            relation: RelationIdentity::from_legacy_name(relation).unwrap(),
            column: (*column).into(),
        });
    }
    StoredRule {
        definition,
        dependencies: Some(dependencies),
        enabled: EventEnableMode::default(),
        condition_plan: None,
        condition_binding: None,
        catalog_oid: Some(31_000),
    }
}

#[test]
fn native_rule_dependencies_match_nested_columns_targets_and_shadowed_event_rows() {
    let cases = [
        rule("INSERT INTO public.target(a) VALUES (new.id)", None, &["public.target"], &[("public.target", "a"), ("public.items", "id")]),
        rule("UPDATE public.target SET a=old.id FROM public.source AS new WHERE target.id=new.id RETURNING target.a", None, &["public.target", "public.source"], &[("public.target", "a"), ("public.target", "id"), ("public.source", "id"), ("public.items", "id")]),
        rule("DELETE FROM public.target WHERE id=old.id RETURNING id", None, &["public.target"], &[("public.target", "id"), ("public.items", "id")]),
        rule("WITH unused AS (SELECT a FROM public.source) SELECT new.id FROM public.target AS new", None, &["public.source", "public.target"], &[("public.source", "a"), ("public.target", "id")]),
        rule("WITH source AS (SELECT 1 AS id) SELECT id FROM source", None, &[], &[]),
        rule("SELECT j.id FROM public.source AS l JOIN public.target AS r USING (id) AS j", None, &["public.source", "public.target"], &[("public.source", "id"), ("public.target", "id")]),
        rule("SELECT row_to_json(t) FROM public.target AS t", None, &["public.target"], &[]),
        rule("SELECT s.tableoid, ctid FROM public.source AS s", None, &["public.source"], &[]),
        rule("SELECT a AS id FROM public.source ORDER BY id", None, &["public.source"], &[("public.source", "a")]),
        rule("NOTHING", Some("EXISTS(WITH x AS (SELECT a FROM public.source) SELECT 1 FROM x WHERE old.id > 0)"), &["public.source"], &[("public.source", "a"), ("public.items", "id")]),
        rule("NOTHING", Some("(old).id > 0"), &[], &[("public.items", "id")]),
        rule("NOTHING", Some("row_to_json(old) IS NOT NULL"), &[], &[]),
        rule("WITH moved AS (DELETE FROM public.source WHERE id=old.id RETURNING a) INSERT INTO public.target(a) SELECT a FROM moved", None, &["public.source", "public.target"], &[("public.source", "id"), ("public.source", "a"), ("public.target", "a"), ("public.items", "id")]),
        rule("INSERT INTO public.target(a) VALUES(new.id) ON CONFLICT(a) DO UPDATE SET b=excluded.a RETURNING new.b", None, &["public.target"], &[("public.target", "a"), ("public.target", "b"), ("public.items", "id")]),
    ];
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for rule in cases {
        let original = serde_json::to_vec(&rule).unwrap();
        validate(&rule, &Catalog, &namespace(), &control)
            .unwrap_or_else(|error| panic!("{:?}: {error}", rule.definition));
        assert_eq!(serde_json::to_vec(&rule).unwrap(), original);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_rule_dependencies_preserve_output_aliases_and_quoted_relation_scopes() {
    let cases = [
        rule("SELECT DISTINCT ON (b) a AS b FROM public.target ORDER BY b", None, &["public.target"], &[("public.target", "a")]),
        rule("SELECT DISTINCT ON (target.b) a AS b FROM public.target ORDER BY target.b", None, &["public.target"], &[("public.target", "a"), ("public.target", "b")]),
        rule("SELECT 1 FROM public.source AS b WHERE EXISTS (SELECT 1 FROM public.\"a.b\" WHERE b.id>0)", None, &["public.source", "public.\"a.b\""], &[("public.source", "id")]),
        rule("SELECT \"a.b\".id FROM public.\"a.b\"", None, &["public.\"a.b\""], &[("public.\"a.b\"", "id")]),
        rule("SELECT \"My Table\".id FROM public.\"My Table\"", None, &["public.\"My Table\""], &[("public.\"My Table\"", "id")]),
        rule("SELECT \"a\"\"b\".id FROM public.\"a\"\"b\"", None, &["public.\"a\"\"b\""], &[("public.\"a\"\"b\"", "id")]),
    ];
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for rule in cases {
        let original = serde_json::to_vec(&rule).unwrap();
        validate(&rule, &Catalog, &namespace(), &control)
            .unwrap_or_else(|error| panic!("{:?}: {error}", rule.definition));
        assert_eq!(serde_json::to_vec(&rule).unwrap(), original);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_rule_dependencies_reject_missing_and_extra_summary_entries() {
    let original = rule(
        "INSERT INTO public.target(a) VALUES (new.id)",
        None,
        &["public.target"],
        &[("public.target", "a"), ("public.items", "id")],
    );
    for damage in [
        "absent",
        "relation",
        "column",
        "extra_relation",
        "extra_column",
        "extra_routine",
    ] {
        let mut rule = original.clone();
        let dependencies = rule.dependencies.as_mut().unwrap();
        match damage {
            "absent" => rule.dependencies = None,
            "relation" => dependencies.relations.clear(),
            "column" => dependencies.columns.clear(),
            "extra_relation" => {
                dependencies
                    .relations
                    .insert(RelationIdentity::new("public", "spare"));
            }
            "extra_column" => {
                dependencies.columns.insert(RuleColumnDependency {
                    relation: RelationIdentity::new("public", "items"),
                    column: "b".into(),
                });
            }
            "extra_routine" => {
                dependencies
                    .routines
                    .insert(crate::catalog::events::RuleRoutineDependency {
                        object_id: Some([7; 16]),
                        name: "public.spare".into(),
                        argument_types: Vec::new(),
                    });
            }
            _ => unreachable!(),
        }
        let before = serde_json::to_vec(&rule).unwrap();
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let error = validate(
            &rule,
            &Catalog,
            &namespace(),
            &ProductionControl::new(&memory, &token, &token),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("rule dependencies"),
            "{damage}: {error}"
        );
        assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_rule_dependency_projection_retires_denial_and_original_cancellation() {
    let rule = rule(
        "INSERT INTO public.target(a) SELECT a FROM public.source WHERE id=new.id",
        None,
        &["public.target", "public.source"],
        &[
            ("public.target", "a"),
            ("public.source", "a"),
            ("public.source", "id"),
            ("public.items", "id"),
        ],
    );
    let namespace = namespace();
    let before = serde_json::to_vec(&rule).unwrap();
    let mut outcomes = [0, 0];
    for limit in [0, 32, 512, 4096, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        match validate(
            &rule,
            &Catalog,
            &namespace,
            &ProductionControl::new(&memory, &token, &token),
        ) {
            Ok(()) => outcomes[0] += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                outcomes[1] += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(outcomes.iter().all(|count| *count > 0));
    for original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let source = CancellationToken::new();
        let caller = CancellationToken::new();
        if original {
            source.cancel();
        } else {
            caller.cancel();
        }
        let error = validate(
            &rule,
            &Catalog,
            &namespace,
            &ProductionControl::new(&memory, &source, &caller),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
}

#[test]
fn native_rule_dependencies_validate_captured_query_columns_independently_of_syntax() {
    let mut original = rule(
        "NOTHING",
        Some("EXISTS(SELECT s.id FROM public.source AS s)"),
        &["public.source"],
        &[("public.source", "id")],
    );
    original.condition_plan = Some(ExpressionPlan::lower(
        original.definition.condition.clone().unwrap(),
    ));
    original.condition_binding = Some(crate::catalog::events::RuleConditionBinding::for_event(
        &["id".into(), "a".into()],
        crate::ast::RuleEvent::Update,
    ));
    for corrupt in [false, true] {
        let mut rule = original.clone();
        if corrupt {
            let crate::plan::RelationalPlan::QueryBlock(block) =
                &mut rule.condition_plan.as_mut().unwrap().subqueries[0].root
            else {
                panic!("query block");
            };
            let ScalarExpr::QualifiedColumn { column, .. } = &mut block.projections[0].expr else {
                panic!("qualified projection");
            };
            *column = "a".into();
        }
        let before = serde_json::to_vec(&rule).unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let result = validate(
            &rule,
            &Catalog,
            &namespace(),
            &ProductionControl::new(&memory, &token, &token),
        );
        assert_eq!(result.is_ok(), !corrupt, "{result:?}");
        assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_rule_dependencies_preserve_captured_source_columns_after_layout_changes() {
    let mut rule = rule(
        "SELECT s.selected FROM public.source AS s(ignored, selected)",
        None,
        &["public.source"],
        &[("public.source", "id")],
    );
    let Statement::Select(select) = &mut rule.definition.actions[0] else {
        panic!("query")
    };
    let Some(crate::ast::FromClause::Table { bound_columns, .. }) = &mut select.from else {
        panic!("source")
    };
    *bound_columns = Some(vec!["dropped".into(), "id".into()]);
    let original = serde_json::to_vec(&rule).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    validate(
        &rule,
        &Catalog,
        &namespace(),
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap();
    assert_eq!(serde_json::to_vec(&rule).unwrap(), original);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_rule_dependencies_match_selected_routine_identity_name_and_signature() {
    let binding = FunctionBinding {
        object_id: Some([3; 16]),
        name: "public.selected".into(),
        argument_types: vec!["int4".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    };
    for source in [false, true] {
        let mut rule = if source {
            rule("SELECT f FROM public.selected(1) AS f", None, &[], &[])
        } else {
            rule(
                "SELECT public.selected(old.id)",
                None,
                &[],
                &[("public.items", "id")],
            )
        };
        let Statement::Select(select) = &mut rule.definition.actions[0] else {
            panic!("query")
        };
        if source {
            let Some(crate::ast::FromClause::Function {
                binding: selected, ..
            }) = &mut select.from
            else {
                panic!("function source")
            };
            *selected = Some(binding.clone());
        } else {
            let crate::ast::Expr::Func {
                binding: selected, ..
            } = &mut select.projections[0].expr
            else {
                panic!("function expression")
            };
            *selected = Some(binding.clone());
        }
        for damage in ["none", "identity", "signature", "name", "missing"] {
            let mut rule = rule.clone();
            let mut dependency = crate::catalog::events::RuleRoutineDependency {
                object_id: binding.object_id,
                name: binding.name.clone(),
                argument_types: binding.argument_types.clone(),
            };
            match damage {
                "identity" => dependency.object_id = Some([4; 16]),
                "signature" => dependency.argument_types[0] = "text".into(),
                "name" => dependency.name = "public.other".into(),
                _ => {}
            }
            if damage != "missing" {
                rule.dependencies
                    .as_mut()
                    .unwrap()
                    .routines
                    .insert(dependency);
            }
            let before = serde_json::to_vec(&rule).unwrap();
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let result = validate(
                &rule,
                &Catalog,
                &namespace(),
                &ProductionControl::new(&memory, &token, &token),
            );
            assert_eq!(
                result.is_ok(),
                damage == "none",
                "source={source}, {damage}: {result:?}"
            );
            assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
            assert_eq!(memory.used(), 0);
        }
    }
}
