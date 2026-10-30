//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, ColumnType, Expr, InternalRelationId, RuleEvent, Statement},
    catalog::{events::StoredRule, roles::RoleReference},
    plan::{CommandPlan, QueryPlan, UnifiedPlan},
    plpgsql::VariableResolver,
    semantics::rules::{
        action_binding::{bind_rule_action, expand_rule_action_row_stars, RuleSourceCatalog},
        analysis::RuleAnalysisContext,
        RuleCatalog,
    },
    RowSchema, SQLError, ScalarExpr,
};
use std::collections::BTreeMap;

mod collations;
mod owned;

struct Catalog(Vec<ColumnDef>);
impl RuleSourceCatalog for Catalog {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        Ok(Some(
            self.0.iter().map(|column| column.name.clone()).collect(),
        ))
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<ColumnDef>, SQLError> {
        Ok(self.0.clone())
    }
}
impl crate::semantics::returning::ReturningCatalog for Catalog {
    fn with_table_row_type_with_control(
        &self,
        _: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        read(&self.0)?;
        Ok(true)
    }

    fn try_describe_table_row_type(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(self.0.clone()))
    }
    fn try_table_columns(&self, _: &str) -> Result<Vec<String>, String> {
        Ok(self.0.iter().map(|column| column.name.clone()).collect())
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
impl Catalog {
    fn analysis(&self) -> RuleAnalysisContext<'_> {
        RuleAnalysisContext {
            rules: self,
            sources: self,
            returning: self,
            domains: self,
            composites: None,
        }
    }
    fn binding(&self) -> crate::binding::BindingContext<'static> {
        let mut binding = crate::binding::fixture::empty_binding_context();
        binding.catalog = crate::binding::fixture::catalog(BTreeMap::from([
            (
                crate::RelationIdentity::new("public", "items"),
                crate::binding::fixture::table_definition(self.0.clone()),
            ),
            (
                crate::RelationIdentity::new("public", "log"),
                crate::binding::fixture::table_definition(self.0.clone()),
            ),
        ]));
        binding
    }
}
fn command(sql: &str) -> Box<CommandPlan> {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    command
}
fn native_action(catalog: &Catalog, sql: &str, action: &Statement, restore: bool) -> UnifiedPlan {
    let command = command(sql);
    let command = if restore {
        serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap()
    } else {
        command
    };
    let (mut resolver, input) = match command.as_ref() {
        CommandPlan::Insert(insert) => {
            let queries = InsertRuleQueries::bind(
                catalog.analysis(),
                insert,
                &|_| Ok("owner".into()),
                &"caller".into(),
            )
            .unwrap();
            let insert = &queries.relations[0];
            let relation = InternalRelationId::allocate();
            let resolver =
                RuleQueryResolver::from_insert(catalog.analysis(), insert, relation).unwrap();
            let input = RuleQueryInput {
                query: *insert.source.clone().unwrap(),
                inline: insert
                    .rule_values_input
                    .as_ref()
                    .is_some_and(crate::plan::RuleValuesInput::is_inline),
                relation,
                condition: None,
                expressions: BTreeMap::new(),
            };
            (resolver, input)
        }
        CommandPlan::Update(update) => mutation_input(catalog, RuleMutation::Update(update)),
        CommandPlan::Delete(delete) => mutation_input(catalog, RuleMutation::Delete(delete)),
        _ => panic!("event mutation"),
    };
    let columns = catalog.0.iter().map(|column| column.name.clone()).collect();
    let bound = bind_rule_action(catalog, action, &columns, &mut resolver).unwrap();
    let bound = if restore {
        serde_json::from_slice(&serde_json::to_vec(&bound).unwrap()).unwrap()
    } else {
        bound
    };
    let input = RuleQueryInput {
        expressions: resolver.expressions,
        ..input
    };
    RuleAction::bind(catalog.analysis(), &|_: &str| false, bound, Some(input))
        .unwrap()
        .lower(&|_: &str| false)
        .unwrap()
}
fn mutation_input(
    catalog: &Catalog,
    mutation: RuleMutation<'_>,
) -> (RuleQueryResolver, RuleQueryInput) {
    let binding = catalog.binding();
    let schema = crate::binding::analyze_source_plan_schema(
        catalog,
        &mutation.target_source(),
        &[],
        &binding,
        None,
    )
    .unwrap();
    let input = MutationRuleInput::bind(
        catalog.analysis(),
        crate::binding::correlation::CorrelationContext {
            routines: catalog,
            catalog: binding.catalog.as_ref(),
            resolution: &binding.resolution,
            scoped_relations: None,
        },
        mutation,
        (&schema, None),
        true,
        &RoleReference::from("caller"),
        &RoleReference::from("owner"),
    )
    .unwrap();
    (input.resolver, input.input)
}

#[test]
fn native_rule_layout_keeps_declared_width_collation_and_whole_row_order() {
    let mut text = ColumnDef::nullable("a", ColumnType::Text);
    text.collation_oid = Some(950);
    let catalog = Catalog(vec![
        ColumnDef::nullable("z", ColumnType::SmallInteger),
        text,
    ]);
    let insert = command("INSERT INTO items SELECT 7, NULL::text");
    let CommandPlan::Insert(insert) = insert.as_ref() else {
        panic!("INSERT")
    };
    let relation = InternalRelationId::allocate();
    let mut resolver =
        RuleQueryResolver::from_insert(catalog.analysis(), insert, relation).unwrap();
    assert_eq!(
        resolver
            .columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    let Expr::Cast { expr, ty, .. } = resolver.rewrite_name("NEW").unwrap().unwrap() else {
        panic!("row cast")
    };
    assert_eq!(ty, "items");
    let Expr::Row(fields) = *expr else {
        panic!("whole row")
    };
    for (field, (_, expected)) in fields.iter().zip(&resolver.columns) {
        assert_eq!(field, expected.as_ref().unwrap());
    }
    for (position, (_, expression)) in resolver.columns.iter().enumerate() {
        let Expr::InternalColumn(reference) = expression.as_ref().unwrap() else {
            panic!("internal input")
        };
        let expression = &resolver.expressions[reference].scalar;
        let empty = RowSchema::default();
        assert_eq!(
            crate::scalar_type(expression, &empty, &[]).unwrap(),
            Some(catalog.0[position].ty.clone())
        );
        assert_eq!(
            crate::schema::ScalarTypeSchema::input_collation(&empty, expression),
            if position == 0 { None } else { Some(950) }
        );
    }
}

#[test]
fn native_rule_inputs_keep_distinct_internal_relations() {
    let catalog = Catalog(vec![ColumnDef::nullable("n", ColumnType::Integer)]);
    let action = crate::compile("SELECT NEW.n").unwrap().remove(0);
    let mut observed = Vec::new();
    for _ in 0..2 {
        let UnifiedPlan::Query(query) =
            native_action(&catalog, "INSERT INTO items SELECT 7", &action, false)
        else {
            panic!("query")
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let crate::plan::SourcePlan::Subquery {
            captured_output: Some(relation),
            ..
        } = block.from.unwrap()
        else {
            panic!("captured input")
        };
        let mut references = Vec::new();
        block.projections[0].expr.visit(&mut |node| {
            if let ScalarExpr::InternalColumn(column) = node {
                references.push(*column);
            }
        });
        assert_eq!(references, [relation.column(0)]);
        observed.push(relation);
    }
    assert_ne!(observed[0], observed[1]);
}
