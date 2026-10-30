//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unevaluated UPDATE and DELETE inputs retain their original namespaces and authority.

use super::{
    assignments, prepend, projection_block, BTreeMap, Expr, InternalRelationId, ProjectionPlan,
    QueryPlan, RelationalPlan, RuleQueryInput, RuleQueryResolver, SQLError, ScalarExpr, SourcePlan,
};
use crate::{
    ast::{BooleanTest, FunctionBinding, FunctionDispatch, FunctionOrderSyntax, RuleEvent},
    binding::correlation::{expression_depends_on_outer_row, CorrelationContext},
    catalog::roles::RoleReference,
    plan::{CtePlan, DeletePlan, ExpressionPlan, UpdatePlan},
    semantics::{rules::analysis::RuleAnalysisContext, view_rewrite::DeferredOuterRewrite},
    ColumnIdentity, RowSchema,
};

#[derive(Clone, Copy)]
pub enum RuleMutation<'a> {
    Update(&'a UpdatePlan),
    Delete(&'a DeletePlan),
}

macro_rules! field {
    ($self:expr, $name:ident) => {
        match $self {
            RuleMutation::Update(plan) => &plan.$name,
            RuleMutation::Delete(plan) => &plan.$name,
        }
    };
}

impl<'a> RuleMutation<'a> {
    pub fn table(self) -> &'a str {
        field!(self, table)
    }

    pub fn event(self) -> RuleEvent {
        match self {
            Self::Update(_) => RuleEvent::Update,
            Self::Delete(_) => RuleEvent::Delete,
        }
    }

    pub fn target_qualifier(self) -> &'a str {
        field!(self, target_qualifier)
    }

    pub fn source(self) -> Option<&'a SourcePlan> {
        field!(self, source).as_deref()
    }

    pub fn predicate(self) -> Option<&'a ScalarExpr> {
        field!(self, predicate).as_ref()
    }

    pub fn subqueries(self) -> &'a [QueryPlan] {
        field!(self, subqueries)
    }

    pub fn ctes(self) -> &'a [CtePlan] {
        field!(self, ctes)
    }

    pub fn returning(self) -> &'a [ProjectionPlan] {
        field!(self, returning)
    }

    pub fn returning_aliases(self) -> &'a crate::ast::ReturningAliases {
        field!(self, returning_aliases)
    }

    pub fn target_source(self) -> SourcePlan {
        SourcePlan::Table {
            name: self.table().to_owned(),
            executable_query: None,
            qualifier: self.target_qualifier().to_owned(),
            alias: field!(self, target_alias).clone(),
            column_aliases: Vec::new(),
            bound_columns: None,
            include_descendants: *field!(self, include_descendants),
        }
    }

    fn relations_bound(self) -> bool {
        *field!(self, relations_bound)
    }

    /// Each conditional INSTEAD rule contributes its own unevaluated negative qualification to the original command.
    pub fn original_condition(
        self,
        context: RuleAnalysisContext<'_>,
        binding: crate::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'_>,
        subject: &RoleReference,
    ) -> Result<Option<ExpressionPlan>, SQLError> {
        let rules = context.rules.capture_rules(self.table(), self.event())?;
        let mut resolver = resolver(context, self, None, None)?;
        let definitions =
            crate::semantics::rules::analysis::rule_returning_columns(context, self.table())?;
        let mut predicates = Vec::new();
        let mut subqueries = Vec::new();
        for rule in rules.iter() {
            let rule = rule?;
            if !rule.definition.instead {
                continue;
            }
            let Some(condition) = resolver.condition(binding, rule, &definitions, subject)? else {
                continue;
            };
            let binding =
                FunctionBinding::dispatched(FunctionDispatch::BooleanTest(BooleanTest::IsNotTrue));
            predicates.push(ScalarExpr::Func {
                name: binding.name.clone(),
                binding: Some(binding),
                args: vec![crate::plan::input_projection::append_expression(
                    &mut subqueries,
                    condition,
                )],
                order_syntax: FunctionOrderSyntax::Ordinary,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            });
        }
        if predicates.is_empty() {
            return Ok(None);
        }
        let expression = ExpressionPlan {
            scalar: ScalarExpr::And(predicates),
            subqueries,
        };
        Ok(Some(expression))
    }
}

/// Raw event attributes and FROM/USING attributes cross separate private boundaries. NEW assignment expressions stay outside those boundaries and are copied at each reference.
pub struct MutationRuleInput {
    pub resolver: RuleQueryResolver,
    pub input: RuleQueryInput,
    pub source_context: Vec<Expr>,
    pub source_context_schema: Option<RowSchema>,
}

impl MutationRuleInput {
    pub fn qualify_update_original(
        context: RuleAnalysisContext<'_>,
        statement: &mut UpdatePlan,
        binding: crate::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'_>,
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        if crate::semantics::rules::analysis::relation_suppresses_original_query(
            context,
            &statement.table,
            RuleEvent::Update,
        )? {
            return Ok(false);
        }
        let condition =
            RuleMutation::Update(statement).original_condition(context, binding, subject)?;
        super::add_native_condition(
            &mut statement.predicate,
            &mut statement.subqueries,
            condition,
        );
        Ok(true)
    }

    pub fn qualify_delete_original(
        context: RuleAnalysisContext<'_>,
        statement: &mut DeletePlan,
        binding: crate::binding::stored_routines::analysis::CatalogRoutineAnalysisContext<'_>,
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        if crate::semantics::rules::analysis::relation_suppresses_original_query(
            context,
            &statement.table,
            RuleEvent::Delete,
        )? {
            return Ok(false);
        }
        let condition =
            RuleMutation::Delete(statement).original_condition(context, binding, subject)?;
        super::add_native_condition(
            &mut statement.predicate,
            &mut statement.subqueries,
            condition,
        );
        Ok(true)
    }

    pub fn bind(
        context: RuleAnalysisContext<'_>,
        correlation: CorrelationContext<'_>,
        statement: RuleMutation<'_>,
        schemas: (&RowSchema, Option<&RowSchema>),
        references_event: bool,
        input_subject: &RoleReference,
        rule_subject: &RoleReference,
    ) -> Result<Self, SQLError> {
        let (target_schema, source_schema) = schemas;
        let relation = InternalRelationId::allocate();
        let source_relation = InternalRelationId::allocate();
        let captured = statement
            .source()
            .zip(source_schema)
            .map(|(source, schema)| {
                capture_source(statement, source, schema, source_relation, input_subject)
            });
        let empty_columns = BTreeMap::new();
        let source_columns = captured
            .as_ref()
            .map_or(&empty_columns, |source| &source.columns);
        let definitions =
            crate::semantics::rules::analysis::rule_returning_columns(context, statement.table())?;
        let target_width = definitions.len();
        let rewrite = input_rewrite(
            statement,
            &definitions,
            source_schema,
            relation,
            source_relation,
            target_schema,
        );
        let mut resolver = resolver(context, statement, Some(relation), Some(&rewrite))?;
        for expression in resolver.expressions.values_mut() {
            bind_positions(expression, relation, source_relation, target_width);
            crate::plan::input_projection::substitute_expression_inputs(
                expression,
                source_columns,
            )?;
            super::values::retain_authority(&mut expression.subqueries, Some(input_subject));
        }
        let mut predicate = statement.predicate().map(|scalar| ExpressionPlan {
            scalar: scalar.clone(),
            subqueries: statement.subqueries().to_vec(),
        });
        let needs_target = references_event
            || predicate.as_ref().is_some_and(|expression| {
                let mut found = false;
                expression.scalar.visit(&mut |node| {
                found |= matches!(node, ScalarExpr::Position(position) if *position < target_width);
            });
                found
            })
            || predicate
                .as_ref()
                .map(|expression| {
                    expression_depends_on_outer_row(
                        correlation,
                        expression,
                        statement.source(),
                        statement.ctes(),
                    )
                })
                .transpose()?
                .unwrap_or(false);
        if let Some(expression) = &mut predicate {
            bind_positions(expression, relation, source_relation, target_width);
            rewrite.bind_expression(expression);
            crate::plan::input_projection::substitute_expression_inputs(
                expression,
                source_columns,
            )?;
            crate::plan::subqueries::prune_expression(expression);
            super::values::retain_authority(&mut expression.subqueries, Some(input_subject));
        }
        let target = needs_target.then(|| {
            target_source(
                statement,
                &definitions,
                target_schema,
                relation,
                rule_subject,
            )
        });
        let mut block = projection_block(Vec::new());
        // FROM/USING is the outer side of the event relation cross product, before mutation de-duplication.
        let (source, source_context, source_context_schema) = captured.map_or_else(
            || (None, Vec::new(), None),
            |source| (Some(source.source), source.context, Some(source.schema)),
        );
        block.from = prepend(target, source);
        super::add_native_condition(&mut block.r#where, &mut block.subqueries, predicate);
        Ok(Self {
            resolver,
            input: RuleQueryInput {
                query: query(block, statement.relations_bound(), input_subject),
                inline: true,
                relation,
                condition: None,
                expressions: BTreeMap::new(),
            },
            source_context,
            source_context_schema,
        })
    }

    pub fn finish(mut self, condition: Option<ExpressionPlan>) -> RuleQueryInput {
        self.input.condition = condition;
        self.input.expressions = self.resolver.expressions;
        self.input
    }
}

struct CapturedMutationSource {
    source: SourcePlan,
    columns: BTreeMap<crate::ast::InternalColumnRef, ExpressionPlan>,
    context: Vec<Expr>,
    schema: RowSchema,
}

fn capture_source(
    statement: RuleMutation<'_>,
    source: &SourcePlan,
    schema: &RowSchema,
    relation: InternalRelationId,
    subject: &RoleReference,
) -> CapturedMutationSource {
    let mut block = projection_block(
        (0..schema.len())
            .map(|position| ProjectionPlan {
                expr: ScalarExpr::Position(position),
                alias: Some(schema.columns()[position].clone()),
            })
            .collect(),
    );
    block.from = Some(source.clone());
    block
        .projections
        .extend(source_whole_rows(schema).map(|qualifier| ProjectionPlan {
            expr: ScalarExpr::Column(qualifier.into()),
            alias: None,
        }));
    block.projections.extend(virtual_projections(schema));
    let mut slots: BTreeMap<_, _> = (0..schema.len())
        .filter_map(|position| schema.physical_slot(position).map(|slot| (slot, position)))
        .collect();
    for (identity, slot) in schema.lookup_aliases() {
        if let Some(slot) = slot {
            slots.entry(slot).or_insert_with(|| {
                let position = block.projections.len();
                block.projections.push(ProjectionPlan {
                    expr: identity.qualifier().map_or_else(
                        || ScalarExpr::Column(identity.column().into()),
                        |qualifier| ScalarExpr::qualified_column(qualifier, identity.column()),
                    ),
                    alias: None,
                });
                position
            });
        }
    }
    let mut columns = BTreeMap::new();
    for (column, slot) in schema.internal_identities() {
        let position = *slots.entry(slot).or_insert_with(|| {
            let position = block.projections.len();
            block.projections.push(ProjectionPlan {
                expr: ScalarExpr::InternalColumn(column),
                alias: None,
            });
            position
        });
        columns.insert(column, input_expression(relation, position));
    }
    block.subqueries = statement.subqueries().to_vec();
    let (schema, context_slots) = schema.canonical_projection();
    let context = context_slots
        .into_iter()
        .map(|slot| Expr::InternalColumn(relation.column(slots[&slot])))
        .collect();
    CapturedMutationSource {
        source: capture(query(block, statement.relations_bound(), subject), relation),
        columns,
        context,
        schema,
    }
}

fn resolver(
    context: RuleAnalysisContext<'_>,
    statement: RuleMutation<'_>,
    relation: Option<InternalRelationId>,
    rewrite: Option<&DeferredOuterRewrite>,
) -> Result<RuleQueryResolver, SQLError> {
    let definitions =
        crate::semantics::rules::analysis::rule_returning_columns(context, statement.table())?;
    let mut resolver = RuleQueryResolver {
        table: statement.table().to_owned(),
        columns: Vec::new(),
        old: BTreeMap::new(),
        expressions: BTreeMap::new(),
    };
    for (position, column) in definitions.iter().enumerate() {
        let old = relation.map_or_else(
            || ScalarExpr::qualified_column(statement.target_qualifier(), &column.name),
            |relation| ScalarExpr::InternalColumn(relation.column(position)),
        );
        let old_reference = InternalRelationId::allocate().column(0);
        resolver
            .old
            .insert(column.name.clone(), Expr::InternalColumn(old_reference));
        resolver.expressions.insert(
            old_reference,
            ExpressionPlan {
                scalar: old.clone(),
                subqueries: Vec::new(),
            },
        );
        let expression = match statement {
            RuleMutation::Update(update) => {
                assignments::update_column_expression(context, update, column, old)
            }
            RuleMutation::Delete(_) => Ok(ExpressionPlan {
                scalar: ScalarExpr::TypedLiteral {
                    value: uqa_core::Value::Null,
                    ty: column.ty.catalog_name(),
                    bound_type: Some(column.ty.clone()),
                    parameter_index: None,
                    collation_oid: column.collation_oid,
                    composite_source: None,
                },
                subqueries: Vec::new(),
            }),
        };
        let expression = expression.map(|mut expression| {
            let metadata = crate::semantics::rules::binding::RuleColumnMetadata {
                ty: column.ty.clone(),
                collation_oid: column.collation_oid,
                uses_document_id: false,
                position,
            };
            expression.scalar = metadata.bind_input(expression.scalar);
            if let Some(rewrite) = rewrite {
                rewrite.bind_expression(&mut expression);
            }
            let reference = InternalRelationId::allocate().column(0);
            resolver.expressions.insert(reference, expression);
            Expr::InternalColumn(reference)
        });
        resolver.columns.push((column.name.clone(), expression));
    }
    Ok(resolver)
}

fn input_expression(relation: InternalRelationId, position: usize) -> ExpressionPlan {
    ExpressionPlan {
        scalar: ScalarExpr::InternalColumn(relation.column(position)),
        subqueries: Vec::new(),
    }
}

fn bind_positions(
    expression: &mut ExpressionPlan,
    target: InternalRelationId,
    source: InternalRelationId,
    target_width: usize,
) {
    crate::plan::rewrite_scalar_expression(&mut expression.scalar, &mut |node| {
        if let ScalarExpr::Position(position) = node {
            *node = ScalarExpr::InternalColumn(if *position < target_width {
                target.column(*position)
            } else {
                source.column(*position - target_width)
            });
        }
    });
}

fn query(
    block: crate::plan::QueryBlockPlan,
    relations_bound: bool,
    subject: &RoleReference,
) -> QueryPlan {
    QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relation_privilege_subject: Some(subject.clone()),
        relations_bound,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(block)),
    }
}

fn capture(query: QueryPlan, relation: InternalRelationId) -> SourcePlan {
    SourcePlan::Subquery {
        captured_output: Some(relation),
        lateral: false,
        body: Box::new(query),
        alias: None,
        column_aliases: Vec::new(),
    }
}

fn input_rewrite(
    statement: RuleMutation<'_>,
    definitions: &[crate::ast::ColumnDef],
    source_schema: Option<&RowSchema>,
    relation: InternalRelationId,
    source_relation: InternalRelationId,
    target_schema: &RowSchema,
) -> DeferredOuterRewrite {
    let mut names = BTreeMap::new();
    names.insert(
        ColumnIdentity::unqualified(statement.target_qualifier()),
        input_expression(relation, definitions.len()),
    );
    for (position, column) in definitions.iter().enumerate() {
        let expression = input_expression(relation, position);
        names.insert(
            ColumnIdentity::qualified(statement.target_qualifier(), &column.name),
            expression.clone(),
        );
        names.insert(ColumnIdentity::unqualified(&column.name), expression);
    }
    capture_virtual_names(&mut names, target_schema, relation, definitions.len() + 1);
    if let Some(schema) = source_schema {
        for (position, qualifier) in source_whole_rows(schema).enumerate() {
            names
                .entry(ColumnIdentity::unqualified(&qualifier))
                .or_insert_with(|| input_expression(source_relation, schema.len() + position));
        }
        for (position, identity) in schema.identities().iter().enumerate() {
            names.insert(
                identity.clone(),
                input_expression(source_relation, position),
            );
            if schema.unqualified_position(identity.column()) == Some(position)
                && !definitions
                    .iter()
                    .any(|column| column.name == identity.column())
            {
                names.insert(
                    ColumnIdentity::unqualified(identity.column()),
                    input_expression(source_relation, position),
                );
            }
        }
        capture_virtual_names(
            &mut names,
            schema,
            source_relation,
            schema.len() + source_whole_rows(schema).count(),
        );
    }
    DeferredOuterRewrite::from_inputs(names)
}

fn target_source(
    statement: RuleMutation<'_>,
    definitions: &[crate::ast::ColumnDef],
    schema: &RowSchema,
    relation: InternalRelationId,
    rule_subject: &RoleReference,
) -> SourcePlan {
    let mut block = projection_block(
        definitions
            .iter()
            .map(|column| ProjectionPlan {
                expr: ScalarExpr::qualified_column(statement.target_qualifier(), &column.name),
                alias: Some(column.name.clone()),
            })
            .collect(),
    );
    block.from = Some(statement.target_source());
    block.projections.push(ProjectionPlan {
        expr: ScalarExpr::Column(statement.target_qualifier().into()),
        alias: None,
    });
    block.projections.extend(virtual_projections(schema));
    capture(
        query(block, statement.relations_bound(), rule_subject),
        relation,
    )
}

fn virtual_projections(schema: &RowSchema) -> impl Iterator<Item = ProjectionPlan> + '_ {
    schema
        .typed_virtual_identities()
        .filter_map(|(identity, _)| {
            identity.qualifier().map(|qualifier| ProjectionPlan {
                expr: ScalarExpr::qualified_column(qualifier, identity.column()),
                alias: None,
            })
        })
}

fn capture_virtual_names(
    names: &mut BTreeMap<ColumnIdentity, ExpressionPlan>,
    schema: &RowSchema,
    relation: InternalRelationId,
    offset: usize,
) {
    for (position, (identity, _)) in schema
        .typed_virtual_identities()
        .filter(|(identity, _)| identity.qualifier().is_some())
        .enumerate()
    {
        let expression = input_expression(relation, offset + position);
        names.insert(identity.clone(), expression.clone());
        names
            .entry(ColumnIdentity::unqualified(identity.column()))
            .or_insert(expression);
    }
}

fn source_whole_rows(schema: &RowSchema) -> impl Iterator<Item = String> {
    schema
        .identities()
        .iter()
        .filter_map(|identity| identity.qualifier().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
}
