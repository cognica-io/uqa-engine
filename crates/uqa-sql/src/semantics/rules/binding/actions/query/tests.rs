//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, ColumnType, RuleEvent},
    catalog::{events::StoredRule, roles::RoleReference},
    semantics::rules::{
        action_binding::RuleSourceCatalog, analysis::RuleAnalysisContext, RuleCatalog,
    },
    RowSchema,
};

struct Catalog;

impl RuleSourceCatalog for Catalog {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        Ok(Some(vec!["n".into()]))
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<ColumnDef>, SQLError> {
        let mut column = ColumnDef::nullable("n", ColumnType::Text);
        column.collation_oid = Some(950);
        Ok(vec![column])
    }
}
impl crate::semantics::returning::ReturningCatalog for Catalog {
    fn try_describe_table_row_type(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(self.rule_relation_definitions("input").unwrap()))
    }
    fn try_table_columns(&self, _: &str) -> Result<Vec<String>, String> {
        Ok(vec!["n".into()])
    }
    fn view_schema(&self, _: &str) -> Result<Option<RowSchema>, SQLError> {
        Ok(None)
    }
}
impl RuleCatalog for Catalog {
    fn relation_has_rules(&self, _: &str) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn resolve_rule_relation(&self, name: &str) -> Result<uqa_core::RelationIdentity, SQLError> {
        uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)
    }
    fn rules_for(&self, _: &str, _: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Ok(Vec::new())
    }
    fn resolve_mutation_target(&self, name: &str, _: bool) -> Result<String, SQLError> {
        Ok(name.into())
    }
}
impl crate::FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl crate::routines::RoutineResolution for Catalog {}

impl crate::catalog::domain::DomainCatalog for Catalog {
    fn domain_by_oid(&self, _: u32) -> Option<crate::catalog::domain::StoredDomain> {
        None
    }
}

#[test]
fn native_rule_expressions_retain_event_collation_in_each_scalar_scope() {
    let context = RuleAnalysisContext {
        rules: &Catalog,
        sources: &Catalog,
        returning: &Catalog,
        domains: &Catalog,
        composites: None,
    };
    for sql in [
        "INSERT INTO input VALUES ('x' COLLATE \"POSIX\")",
        "INSERT INTO input SELECT 'x' COLLATE \"POSIX\"",
    ] {
        let UnifiedPlan::Command(command) =
            UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("command")
        };
        let CommandPlan::Insert(insert) = command.as_ref() else {
            panic!("INSERT")
        };
        let queries = InsertRuleQueries::bind(
            context,
            insert,
            &|_| Ok(RoleReference::from("owner")),
            &RoleReference::from("caller"),
        )
        .unwrap();
        let insert = &queries.relations[0];
        let relation = InternalRelationId::allocate();
        let mut resolver = RuleQueryResolver::from_insert(context, insert, relation).unwrap();
        let action = crate::compile("SELECT NEW.n, NEW.n COLLATE \"POSIX\", (SELECT NEW.n)")
            .unwrap()
            .remove(0);
        let bound = crate::semantics::rules::action_binding::bind_rule_action(
            &Catalog,
            &action,
            &BTreeSet::new(),
            &mut resolver,
        )
        .unwrap();
        let action = RuleAction::bind(
            context,
            &|_: &str| false,
            bound,
            Some(RuleQueryInput {
                query: *insert.source.clone().unwrap(),
                inline: insert
                    .rule_values_input
                    .as_ref()
                    .is_some_and(crate::plan::RuleValuesInput::is_inline),
                relation,
                condition: None,
                expressions: resolver.expressions,
            }),
        )
        .unwrap();
        let plan = action.lower(&|_: &str| false).unwrap();
        let UnifiedPlan::Query(query) = plan else {
            panic!("query")
        };
        let schema = crate::binding::analyze_query_plan_schema(
            &Catalog,
            &query,
            &[],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}; {query:?}"));
        assert_eq!(
            (0..3)
                .map(|i| schema.column_collation(i))
                .collect::<Vec<_>>(),
            vec![Some(950), Some(951), Some(950)],
            "{sql}"
        );
    }
}

struct RuleContracts(Vec<StoredRule>);

#[test]
fn mutation_inputs_keep_native_namespaces_and_event_collations() {
    let context = RuleAnalysisContext {
        rules: &Catalog,
        sources: &Catalog,
        returning: &Catalog,
        domains: &Catalog,
        composites: None,
    };
    let mut binding = crate::binding::fixture::empty_binding_context();
    binding.catalog = crate::binding::fixture::catalog(BTreeMap::from([(
        crate::RelationIdentity::new("public", "input"),
        crate::binding::fixture::table_definition(
            Catalog.rule_relation_definitions("input").unwrap(),
        ),
    )]));
    for expression in [
        "'x' COLLATE \"POSIX\"",
        "(SELECT 'x' COLLATE \"POSIX\")",
        "(input).n",
        "CASE WHEN input.tableoid=1::oid THEN 'x' ELSE 'y' END",
        "(SELECT CASE WHEN input.tableoid=1::oid THEN 'x' ELSE 'y' END)",
    ] {
        let UnifiedPlan::Command(command) = UnifiedPlan::lower(
            crate::compile(&format!("UPDATE input SET n={expression}"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("command")
        };
        let CommandPlan::Update(statement) = command.as_ref() else {
            panic!("UPDATE")
        };
        let mutation = mutation::RuleMutation::Update(statement);
        let schema = crate::binding::analyze_source_plan_schema(
            &Catalog,
            &mutation.target_source(),
            &[],
            &binding,
            None,
        )
        .unwrap();
        let mut input = mutation::MutationRuleInput::bind(
            context,
            crate::binding::correlation::CorrelationContext {
                routines: &Catalog,
                catalog: binding.catalog.as_ref(),
                resolution: &binding.resolution,
                scoped_relations: None,
            },
            mutation,
            (&schema, None),
            true,
            &"caller".into(),
            &"owner".into(),
        )
        .unwrap();
        let action = crate::compile("SELECT OLD.n, NEW.n COLLATE \"POSIX\", (SELECT NEW.n)")
            .unwrap()
            .remove(0);
        let bound = crate::semantics::rules::action_binding::bind_rule_action(
            &Catalog,
            &action,
            &BTreeSet::new(),
            &mut input.resolver,
        )
        .unwrap();
        let action =
            RuleAction::bind(context, &|_: &str| false, bound, Some(input.finish(None))).unwrap();
        let UnifiedPlan::Query(query) = action.lower(&|_: &str| false).unwrap() else {
            panic!("query")
        };
        let output =
            crate::binding::analyze_query_plan_schema(&Catalog, &query, &[], &binding, None)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
        assert_eq!(
            (0..3)
                .map(|position| output.column_collation(position))
                .collect::<Vec<_>>(),
            vec![Some(950), Some(951), Some(950)],
            "{expression}"
        );
    }
}

impl RuleCatalog for RuleContracts {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        Ok(self.0.iter().any(|rule| rule.definition.table == table))
    }
    fn resolve_rule_relation(&self, table: &str) -> Result<uqa_core::RelationIdentity, SQLError> {
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)
    }
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Ok(self
            .0
            .iter()
            .filter(|rule| rule.definition.table == table && rule.definition.event == event)
            .cloned()
            .collect())
    }
    fn resolve_mutation_target(&self, name: &str, _: bool) -> Result<String, SQLError> {
        Ok(name.into())
    }
}

fn rule_contracts(rules: &[&str]) -> RuleContracts {
    RuleContracts(
        rules
            .iter()
            .map(|sql| {
                let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0)
                else {
                    panic!("rule")
                };
                StoredRule {
                    definition,
                    enabled: crate::ast::EventEnableMode::Origin,
                    condition_plan: None,
                    condition_binding: None,
                    dependencies: None,
                    catalog_oid: None,
                }
            })
            .collect(),
    )
}

#[test]
fn set_operation_actions_validate_the_original_insert_query_shape() {
    let rules = rule_contracts(&["CREATE RULE route AS ON INSERT TO public.input DO ALSO INSERT INTO public.output SELECT 1 UNION ALL SELECT 2"]);
    for (input, expected) in [
        ("INSERT INTO public.input VALUES(1)", None),
        ("INSERT INTO public.input VALUES(1),(2)", Some("0A000")),
        ("INSERT INTO public.input SELECT 1", Some("0A000")),
        (
            "INSERT INTO public.input SELECT 1 WHERE false",
            Some("0A000"),
        ),
    ] {
        let UnifiedPlan::Command(command) =
            UnifiedPlan::lower(crate::compile(input).unwrap().remove(0))
        else {
            panic!("command")
        };
        let CommandPlan::Insert(insert) = command.as_ref() else {
            panic!("INSERT")
        };
        let result = crate::semantics::rules::validate_insert_rule_query_contract(
            &rules,
            "public.input",
            insert,
            |_, _| Ok(None),
        );
        assert_eq!(
            result.as_ref().err().and_then(SQLError::sqlstate),
            expected,
            "{input}: {result:?}"
        );
    }
}

#[test]
fn nested_set_operation_contracts_precede_input_evaluation_and_follow_views() {
    let rules = rule_contracts(&[
        "CREATE RULE route AS ON INSERT TO public.input DO ALSO INSERT INTO public.hop VALUES(NEW.n)",
        "CREATE RULE hop AS ON INSERT TO public.hop DO ALSO INSERT INTO public.output SELECT 1 UNION ALL SELECT 2",
    ]);
    for table in ["public.input", "public.view"] {
        let UnifiedPlan::Command(command) = UnifiedPlan::lower(
            crate::compile(&format!("INSERT INTO {table} VALUES(1/0),(2)"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("command")
        };
        let CommandPlan::Insert(insert) = command.as_ref() else {
            panic!("INSERT")
        };
        let error = crate::semantics::rules::validate_insert_rule_query_contract(
            &rules,
            table,
            insert,
            |table, _| Ok((table == "public.view").then(|| "public.input".into())),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
    }
}
