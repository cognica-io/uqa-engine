//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The parameters of a SQL routine as the outermost scope of the names in its body, resolved as `PostgreSQL`'s `sql_fn_post_column_ref` resolves them: a name is a parameter only when no column of any query level and no relation takes it, so a column of a queried table shadows the parameter of the same name, which the routine's own name then qualifies.

use super::{BindingContext, QueryPlan, RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope};
use crate::ast::{ColumnType, InternalRelationId};
use crate::catalog::resolution::RelationLookupMode;
use crate::plan::{CommandPlan, ExpressionPlan, ProjectionPlan, UnifiedPlan};
use crate::routines::RoutineResolution;
use std::sync::LazyLock;

/// The opaque relation whose attributes mark the parameter slots of every row scope the parameter layer reaches, so that a name resolves to a parameter only through the layer itself.
static ROUTINE_PARAMETERS: LazyLock<InternalRelationId> =
    LazyLock::new(InternalRelationId::allocate);

/// The parameters a SQL routine's body can name, in positional order.
#[derive(Clone)]
pub struct RoutineParameterScope {
    /// The routine's unqualified name, which qualifies its parameter names.
    function: String,
    /// The parameter names; an unnamed parameter has an empty name and is reachable only by position.
    names: Vec<String>,
    schema: RowSchema,
}

impl RoutineParameterScope {
    /// The parameters `names`, typed by `types`, of the routine whose unqualified name is `function`.
    #[must_use]
    pub fn new(function: &str, names: Vec<String>, types: Vec<Option<ColumnType>>) -> Self {
        let visible = RowSchema::with_qualified_types(function, names.clone(), types.clone());
        let marked = RowSchema::with_internal_relation_types(*ROUTINE_PARAMETERS, types);
        Self {
            function: function.to_string(),
            names,
            schema: RowSchema::with_trailing_internal_aliases(&visible, &marked),
        }
    }

    /// The parameters as a row scope, the outermost scope of every name in the body.
    #[must_use]
    pub const fn schema(&self) -> &RowSchema {
        &self.schema
    }

    /// The position of the parameter that occupies `slot` of `schema`.
    fn position_at(schema: &RowSchema, slot: usize) -> Option<usize> {
        schema
            .unique_internal_column_for_slot(slot)
            .filter(|column| column.relation() == *ROUTINE_PARAMETERS)
            .map(crate::ast::InternalColumnRef::attribute)
    }

    /// Whether the parameter layer is part of `schema`.
    fn reaches(schema: &RowSchema) -> bool {
        schema.internal_slot(ROUTINE_PARAMETERS.column(0)).is_some()
    }

    /// The position of the parameter that `expression` names in `schema`, or `None` when it names a column, a relation, or nothing the layer holds.
    fn parameter(&self, expression: &ScalarExpr, schema: &RowSchema) -> Option<usize> {
        match expression {
            ScalarExpr::Column(name) => {
                let position = Self::position_at(schema, schema.column_slot(name)?)?;
                // A relation of that name makes the name a whole-row reference, which the parser tries before the parameters.
                (!self.names_relation(schema, name)).then_some(position)
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                match schema.qualified_slot(qualifier, column) {
                    Some(slot) => Self::position_at(schema, slot),
                    // A relation that takes the routine's name but lacks the column leaves the name to the parameter, since the parser finds no column for it.
                    None if *qualifier == self.function && Self::reaches(schema) => self
                        .names
                        .iter()
                        .position(|name| !name.is_empty() && name == column),
                    None => None,
                }
            }
            _ => None,
        }
    }

    /// Whether `name` is a relation visible in `schema`. The layer's own qualifier, the routine's name, is visible only where no relation takes that name.
    fn names_relation(&self, schema: &RowSchema, name: &str) -> bool {
        schema.has_qualifier(name)
            && (name != self.function
                || !self.names.iter().any(|parameter| {
                    schema
                        .qualified_slot(name, parameter)
                        .and_then(|slot| Self::position_at(schema, slot))
                        .is_some()
                }))
    }
}

/// The output name each select list item takes from the column it names, which the item keeps when the name turns out to be a parameter: `PostgreSQL` names the output column of a parameter reference after the reference as written.
pub(super) fn column_labels(projections: &[ProjectionPlan]) -> Vec<Option<String>> {
    projections
        .iter()
        .map(|projection| match &projection.expr {
            ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. }
                if projection.alias.is_none() =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .collect()
}

/// Name each select list item that `labels` took from a column name and that now refers to a parameter after that column name.
pub(super) fn keep_column_labels(projections: &mut [ProjectionPlan], labels: Vec<Option<String>>) {
    for (projection, label) in projections.iter_mut().zip(labels) {
        if let Some(label) = label {
            if matches!(projection.expr, ScalarExpr::Param(_)) {
                projection.alias = Some(label);
            }
        }
    }
}

impl SchemaScope {
    /// Resolve a name at the point ordered semantic analysis visits it.
    pub(super) fn routine_parameter_reference(
        &self,
        expression: &ScalarExpr,
        schema: &RowSchema,
    ) -> Option<usize> {
        self.routine_parameters
            .as_ref()
            .and_then(|parameters| parameters.parameter(expression, schema))
            .map(|position| position + 1)
    }

    /// Replace each reference in `expression` that resolves to a parameter of the routine whose body is bound with the positional parameter it names. `schema` is the row scope `expression` resolves against.
    pub(super) fn canonicalize_routine_parameters(
        &self,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
    ) {
        let Some(parameters) = self.routine_parameters.as_ref() else {
            return;
        };
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if let Some(position) = parameters.parameter(node, schema) {
                *node = ScalarExpr::Param(position + 1);
            }
        });
    }

    /// Walk one statement with `outer`, the parameters of the routine whose body it belongs to, as its outermost scope.
    pub(super) fn bind_statement_parameters(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &mut UnifiedPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let command = match plan {
            UnifiedPlan::Query(query) => {
                return self.bind_query_parameters(routines, query, params, outer);
            }
            UnifiedPlan::Command(command) => command.as_mut(),
        };
        match command {
            CommandPlan::Explain { body, .. } => {
                self.bind_statement_parameters(routines, body, params, outer)
            }
            CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => {
                self.bind_query_parameters(routines, query, params, outer)
            }
            CommandPlan::Call { args, .. } => {
                for argument in args {
                    self.bind_expression_parameters(routines, argument, params, outer)?;
                }
                Ok(())
            }
            command if command.mutation_target().is_some() => {
                self.set_command_lookup_mode(command);
                self.bind_command_routines_for_storage(routines, command, params, outer)
            }
            // A utility statement is not analyzed with the routine's parameters: `PostgreSQL` runs it as written.
            _ => Ok(()),
        }
    }

    /// Resolve the parameter references of a query, looking its relations up as bound identities or by name as its `relations_bound` flag records.
    fn bind_query_parameters(
        &mut self,
        routines: &dyn RoutineResolution,
        query: &mut QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let previous = self.resolution.set_lookup_mode(if query.relations_bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        });
        let result = self
            .bind_query_routines_for_storage(routines, query, params, outer)
            .map(|_| ());
        self.resolution.set_lookup_mode(previous);
        result
    }

    fn bind_expression_parameters(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &mut ExpressionPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        for subquery in &mut expression.subqueries {
            self.bind_query_parameters(routines, subquery, params, outer)?;
        }
        let schema = outer.cloned().unwrap_or_default();
        self.canonicalize_routine_parameters(&mut expression.scalar, &schema);
        self.resolve_variable_sites(&mut expression.scalar, &schema);
        Ok(())
    }
}

/// Resolve the names in one statement of a SQL routine's body that refer to the routine's parameters, against the catalog that `ctes` describes, as `PostgreSQL`'s parser resolves them with the hooks `sql_fn_parser_setup` installs. The routine calls the statement makes are left to its analysis.
pub fn bind_routine_parameter_references(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    parameters: &RoutineParameterScope,
) -> Result<(), SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.routine_parameters = Some(parameters.clone());
    scope.binds_routine_identities = false;
    // Parameter lookup must retain the written expression sites for stored-body binding.
    scope.preserve_syntax_shape = true;
    scope.bind_statement_parameters(routines, plan, params, Some(parameters.schema()))
}
