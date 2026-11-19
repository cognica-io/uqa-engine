//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact routine binding for catalog-owned statements.

use crate::ast::FunctionBinding;
use crate::plan::{
    AccessPathPlan, CommandPlan, ComputePlan, ConflictActionPlan, CtePlan, JoinExecutionStrategy,
    ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::SQLError;
use crate::{RowSchema, ScalarExpr};

use crate::{binding::context::BindingContext, routines::RoutineResolution};
use std::borrow::Cow;
use uqa_core::Value;

/// Routine metadata and immutable namespace inputs for a stored statement.
pub struct CatalogRoutineContext<'a, 'q> {
    pub routines: &'a dyn RoutineResolution,
    pub binding: &'a BindingContext<'q>,
}

/// The command projection needs only a table's analysis row. Each binding input supplies its original namespace and catalog.
trait StatementSource {
    fn schema(&self, source: &SourcePlan) -> Result<RowSchema, SQLError>;
}
impl StatementSource for CatalogRoutineContext<'_, '_> {
    fn schema(&self, source: &SourcePlan) -> Result<RowSchema, SQLError> {
        crate::binding::analyze_source_plan_schema(self.routines, source, &[], self.binding, None)
    }
}
struct RetainedSource<'a> {
    routines: &'a dyn RoutineResolution,
    snapshot: &'a super::production::ControlledBindingSnapshot,
    control: &'a uqa_core::memory::ProductionControl<'a>,
}
impl StatementSource for RetainedSource<'_> {
    fn schema(&self, source: &SourcePlan) -> Result<RowSchema, SQLError> {
        super::production::analyze_source_plan_schema_with_control(
            self.routines,
            source,
            &[],
            self.snapshot.context_with_control(self.control)?,
            None,
            self.control,
        )
    }
}

pub struct BoundStatementRoutines {
    pub query: Option<QueryPlan>,
    /// What binding recorded for the statement's syntax, in syntax order.
    pub sites: super::syntax_sites::SyntaxSites,
    /// The columns a query statement returns; a command's binding query is not its result.
    pub query_output: Option<RowSchema>,
}

/// New syntax records its selected callable. Loading existing syntax preserves an unqualified call that still resolves through the runtime registry before its ordinary builtin fallback.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StoredCallOrigin {
    Definition,
    Persisted,
}

impl StoredCallOrigin {
    pub(crate) fn keeps_runtime_lookup(
        self,
        name: &str,
        original: Option<&FunctionBinding>,
        selected: &FunctionBinding,
    ) -> bool {
        self.keeps_unqualified_runtime_lookup(original, selected)
            && uqa_core::RelationIdentity::try_borrowed_reference(name)
                .is_ok_and(|identity| identity.schema.is_none())
    }

    pub(crate) fn keeps_unqualified_runtime_lookup(
        self,
        original: Option<&FunctionBinding>,
        selected: &FunctionBinding,
    ) -> bool {
        self == Self::Persisted
            && original.is_none()
            && selected.builtin
            && selected.object_id.is_none()
            && selected.invocation.is_none()
            && selected.value_binding.is_none()
            && selected.resolution_error.is_none()
            && selected.dispatch.is_none()
            && selected.input_collation.is_none()
            && !selected.is_polymorphic_builtin_syntax()
    }
}

#[derive(Debug, Clone)]
pub struct BoundRoutineReference {
    pub name: String,
    pub binding: Option<FunctionBinding>,
}

mod command;
use command::command_statement_query;

/// Bind a copy of a catalog-owned statement lowered from its stored syntax; `params` types the positional parameters its syntax references.
pub fn bind_catalog_statement_routines(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &UnifiedPlan,
    params: &[crate::SQLParam],
) -> Result<BoundStatementRoutines, SQLError> {
    bind_catalog_statement(context, plan, None, |query, outer| {
        crate::binding::bind_syntax_query_plan_routines(
            context.routines,
            query,
            params,
            context.binding,
            outer,
        )
    })
}

/// Retain the original syntax correspondence after full statement input analysis has converted literals in a separate plan.
pub(crate) fn bind_prepared_catalog_statement_routines(
    context: &CatalogRoutineContext<'_, '_>,
    syntax: &UnifiedPlan,
    prepared: &UnifiedPlan,
    params: &[crate::SQLParam],
) -> Result<BoundStatementRoutines, SQLError> {
    bind_catalog_statement(context, syntax, Some(prepared), |query, outer| {
        crate::binding::bind_syntax_query_plan_routines(
            context.routines,
            query,
            params,
            context.binding,
            outer,
        )
    })
}

pub(super) fn bind_catalog_statement_composite_inputs(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &UnifiedPlan,
    params: &[crate::SQLParam],
    outer: Option<&RowSchema>,
) -> Result<BoundStatementRoutines, SQLError> {
    bind_catalog_statement(context, plan, None, |query, command_outer| {
        let mut scope = super::SchemaScope::for_analysis(context.binding)?;
        scope.scalar_binding = super::ScalarBindingMode::CompositeInputs;
        scope.preserve_syntax_shape = true;
        scope.stored_expression_outer = outer.cloned();
        let combined = match (command_outer, outer) {
            (Some(left), Some(right)) => Some(RowSchema::join(left, right, [])),
            (Some(schema), None) | (None, Some(schema)) => Some(schema.clone()),
            (None, None) => None,
        };
        scope.bind_query_routines_for_storage(context.routines, query, params, combined.as_ref())
    })
}

type CatalogQuery<'a> = (Cow<'a, QueryPlan>, Option<RowSchema>);

fn catalog_query<'a>(
    context: &impl StatementSource,
    plan: &'a UnifiedPlan,
) -> Result<Option<CatalogQuery<'a>>, SQLError> {
    match plan {
        UnifiedPlan::Query(query) => Ok(Some((Cow::Borrowed(query), None))),
        UnifiedPlan::Command(command) => Ok(command_statement_query(context, command)?
            .map(|(query, outer)| (Cow::Owned(query), outer))),
    }
}

fn bind_catalog_statement(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &UnifiedPlan,
    prepared: Option<&UnifiedPlan>,
    bind: impl FnOnce(&mut QueryPlan, Option<&RowSchema>) -> Result<RowSchema, SQLError>,
) -> Result<BoundStatementRoutines, SQLError> {
    let lowered = catalog_query(context, plan)?;
    let Some((lowered, outer)) = lowered else {
        return Ok(BoundStatementRoutines {
            query: None,
            sites: super::syntax_sites::SyntaxSites::default(),
            query_output: None,
        });
    };
    let mut query = match prepared {
        Some(prepared) => catalog_query(context, prepared)?
            .ok_or_else(|| {
                SQLError::Internal(
                    "prepared catalog statement lost its analyzed expressions".into(),
                )
            })?
            .0
            .into_owned(),
        None => lowered.as_ref().clone(),
    };
    // Syntax sites read the unchanged scalar positions. Only the private query being analyzed needs the stored relation lookup mode.
    if matches!(prepared.unwrap_or(plan), UnifiedPlan::Query(_)) {
        mark_query_relations_bound(&mut query);
    }
    let output = bind(&mut query, outer.as_ref())?;
    let sites = if matches!(plan, UnifiedPlan::Command(_)) {
        super::syntax_sites::command_query_syntax_sites(&lowered, &query)?
    } else {
        super::syntax_sites::query_syntax_sites(&lowered, &query)?
    };
    Ok(BoundStatementRoutines {
        query: Some(query),
        sites,
        query_output: matches!(plan, UnifiedPlan::Query(_)).then_some(output),
    })
}

/// Bind the parameter-resolved statement in place. A private projection holds only the original written positions needed to pair each selected identity back to stored syntax.
pub(crate) async fn statement_syntax_sites_from(
    routines: &dyn RoutineResolution,
    mut plan: crate::plan::copy::stored_plan::BoundPlan,
    params: &[crate::SQLParam],
    snapshot: &super::production::ControlledBindingSnapshot,
    control: &uqa_core::memory::ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<uqa_core::memory::Produced<super::syntax_sites::SyntaxSites>, SQLError> {
    use crate::plan::copy::stored_plan::BindingSyntax;
    let catalog = RetainedSource {
        routines,
        snapshot,
        control,
    };
    let (statement, workspace) = plan.binding_parts();
    let command = matches!(statement, UnifiedPlan::Command(_));
    let mut adapter = None;
    let (query, outer) = match statement {
        UnifiedPlan::Query(query) => (query.as_mut(), None),
        UnifiedPlan::Command(command) => {
            let Some((query, outer)) =
                command::project_from(&catalog, command, control, input).await?
            else {
                plan.retire(control, input).await?;
                return Ok(control.finish(
                    super::syntax_sites::SyntaxSites::default(),
                    control.empty_reservation(),
                )?);
            };
            (adapter.insert(workspace.query(query)).as_mut(), Some(outer))
        }
    };
    let original = workspace.query(query.capture_syntax_before_binding(control, input).await?);
    if !command {
        mark_query_relations_bound(query);
    }
    let mut scope = super::SchemaScope::for_analysis_with_control(
        snapshot.context_with_control(control)?,
        control,
    )?;
    scope.preserve_syntax_shape = true;
    StoredCallOrigin::Definition
        .bind_query_in(&mut scope, routines, query, params, outer.as_ref(), input)
        .await?;
    let sites = super::syntax_sites::query_syntax_sites_with_work(
        &original, query, command, control, input,
    )
    .await?;
    original.retire(control, input).await?;
    if let Some(adapter) = adapter {
        adapter.retire(control, input).await?;
    }
    plan.retain_storage(scope.stored_binding_memory.take().into());
    plan.retire(control, input).await?;
    Ok(sites)
}

pub fn mark_catalog_statement_relations_bound(plan: &mut UnifiedPlan) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => mark_query_relations_bound(query),
        UnifiedPlan::Command(command) => match command.as_mut() {
            CommandPlan::Insert(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
                if let Some(source) = &mut plan.source {
                    mark_query_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Update(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
                if let Some(source) = &mut plan.source {
                    mark_source_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Delete(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
                if let Some(source) = &mut plan.source {
                    mark_source_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Notify { .. } => {}
            CommandPlan::Merge(plan) => {
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
                mark_source_relations_bound(&mut plan.source);
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            _ => {
                return Err(SQLError::Internal(
                    "catalog-owned statement lowered to an unsupported command".into(),
                ));
            }
        },
    }
    Ok(())
}

fn statement_target_outer_schema(
    context: &impl StatementSource,
    table: &str,
    target_qualifier: &str,
    aliases: &crate::ast::ReturningAliases,
) -> Result<RowSchema, SQLError> {
    let target = statement_target_schema(context, table, target_qualifier)?;
    Ok(crate::semantics::returning_expression_schema(
        &target,
        target_qualifier,
        aliases,
        None,
    ))
}

fn statement_target_schema(
    context: &impl StatementSource,
    table: &str,
    target_qualifier: &str,
) -> Result<RowSchema, SQLError> {
    let target = context.schema(&SourcePlan::Table {
        executable_query: None,
        bound_columns: None,
        name: table.to_string(),
        qualifier: target_qualifier.to_string(),
        alias: None,
        column_aliases: Vec::new(),
        include_descendants: true,
    })?;
    Ok(
        RowSchema::with_types(target.columns().to_vec(), target.column_types().to_vec())
            .with_column_collations(
                (0..target.len()).map(|position| target.column_collation(position)),
            ),
    )
}

/// Routine identities of a bound stored expression in syntax order.
pub fn collect_expression_routine_references(
    expression: &crate::plan::ExpressionPlan,
) -> Result<Vec<BoundRoutineReference>, SQLError> {
    Ok(super::syntax_sites::expression_syntax_sites(expression, expression)?.routines)
}

fn mark_cte_relations_bound(body: &mut crate::plan::CtePlanBody) {
    match body {
        crate::plan::CtePlanBody::Query(query) => mark_query_relations_bound(query),
        crate::plan::CtePlanBody::Command(command) => {
            match command.as_mut() {
                CommandPlan::Insert(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                CommandPlan::Update(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                CommandPlan::Delete(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                _ => {}
            }
            if let Some(ctes) = command.ctes_mut() {
                for cte in ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
            }
            for query in command.query_inputs_mut() {
                mark_query_relations_bound(query);
            }
            if let Some(source) = command.source_input_mut() {
                mark_source_relations_bound(source);
            }
        }
    }
}

pub(crate) fn mark_query_relations_bound(query: &mut QueryPlan) {
    query.relations_bound = true;
    for cte in &mut query.ctes {
        mark_cte_relations_bound(&mut cte.body);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                mark_source_relations_bound(source);
            }
            for subquery in &mut block.subqueries {
                mark_query_relations_bound(subquery);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            mark_query_relations_bound(left);
            mark_query_relations_bound(right);
            for subquery in subqueries {
                mark_query_relations_bound(subquery);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                mark_query_relations_bound(subquery);
            }
        }
    }
}

fn mark_source_relations_bound(source: &mut SourcePlan) {
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join { left, right, .. } => {
            mark_source_relations_bound(left);
            mark_source_relations_bound(right);
        }
        SourcePlan::Subquery { body, .. } => mark_query_relations_bound(body),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

pub mod analysis;
