//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The parameters a SQL routine's body names, by name or by position, as `PostgreSQL`'s parser resolves them in each statement of the body.

use super::{
    body_validation::routine_parameter_values,
    compilation::{lower_sql_body_statement, RoutineCompilationContext},
    routine_local_name,
};
use crate::{
    ast::{CreateFunction, Expr, FunctionBody, FunctionParam, FunctionParamMode, Statement},
    binding::{bind_routine_parameter_references, RoutineParameterScope},
    catalog::stored_ast::{visit_stored_statement_expressions, visit_stored_statement_projections},
    plan::UnifiedPlan,
    SQLError, SQLParam, ScalarExpr,
};
use std::cell::Cell;
use std::collections::BTreeSet;

/// The parameters a SQL body can name: the routine's input parameters, which `get_func_input_arg_names` lists.
#[must_use]
pub fn sql_body_parameters(def: &CreateFunction) -> Vec<&FunctionParam> {
    def.params
        .iter()
        .filter(|parameter| is_sql_body_parameter(parameter))
        .collect()
}

/// Whether the body can name `parameter`: an input parameter. A procedure's output parameter takes a placeholder in `CALL` but is not a parameter of its body.
#[must_use]
pub const fn is_sql_body_parameter(parameter: &FunctionParam) -> bool {
    matches!(
        parameter.mode,
        FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
    )
}

/// The scope of the parameters a SQL body names, typed as `params` types them.
pub fn sql_body_parameter_scope(
    def: &CreateFunction,
    params: &[SQLParam],
) -> Result<RoutineParameterScope, SQLError> {
    Ok(RoutineParameterScope::new(
        &routine_local_name(&def.name)?,
        sql_body_parameters(def)
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        params
            .iter()
            .map(|param| param.declared_scalar_type().cloned())
            .collect(),
    ))
}

/// Record in the statements of a SQL-standard body each name that resolves to a parameter, as the positional parameter it names, so that a later change to the relations the body reads cannot take the name: `PostgreSQL` stores the analyzed statements, in which a parameter reference stays a parameter. Returns whether a statement changed.
pub fn record_sql_standard_body_parameters(
    context: &RoutineCompilationContext<'_>,
    def: &mut CreateFunction,
) -> Result<bool, SQLError> {
    let FunctionBody::Statements(statements) = &def.body else {
        return Ok(false);
    };
    let params = routine_parameter_values(context.types, def);
    let recording = ParameterRecording {
        context,
        scope: sql_body_parameter_scope(def, &params)?,
        params,
        sites: ParameterSites {
            function: routine_local_name(&def.name)?,
            names: sql_body_parameters(def)
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
        },
    };
    let mut recorded = statements.clone();
    let mut changed = false;
    for statement in &mut recorded {
        changed |= recording.record(statement)?;
    }
    if changed {
        def.body = FunctionBody::Statements(recorded);
    }
    Ok(changed)
}

/// The names in a body that could refer to the routine's parameters.
struct ParameterSites {
    /// The routine's unqualified name, which qualifies its parameter names.
    function: String,
    names: Vec<String>,
}

impl ParameterSites {
    /// The position of the parameter `expression` could refer to: a parameter name, alone or qualified by the routine's name.
    fn position(&self, expression: &Expr) -> Option<usize> {
        let name = match expression {
            Expr::Column(name) => name,
            Expr::QualifiedColumn { qualifier, column } if *qualifier == self.function => column,
            _ => return None,
        };
        self.names
            .iter()
            .position(|candidate| !candidate.is_empty() && candidate == name)
    }

    /// The parameter positions of the names in `statement` that could refer to parameters, in the order the stored statement visitor reaches them.
    fn collect(&self, statement: &mut Statement) -> Result<Vec<usize>, SQLError> {
        let mut positions = Vec::new();
        visit_stored_statement_expressions(statement, &mut |expression| {
            positions.extend(self.position(expression));
            Ok(())
        })?;
        Ok(positions)
    }

    /// Replace the names whose ordinals `chosen` holds with the parameters they name. A select list item that is such a name keeps its output name as an alias, so that the statement's output columns keep their names.
    fn replace(&self, statement: &mut Statement, chosen: &BTreeSet<usize>) -> Result<(), SQLError> {
        let ordinal = Cell::new(0);
        visit_stored_statement_projections(
            statement,
            &mut |projection| {
                let name = match &projection.expr {
                    Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => name,
                    _ => return Ok(()),
                };
                if projection.alias.is_none()
                    && self.position(&projection.expr).is_some()
                    && chosen.contains(&ordinal.get())
                {
                    projection.alias = Some(name.clone());
                }
                Ok(())
            },
            &mut |expression| {
                if let Some(position) = self.position(expression) {
                    if chosen.contains(&ordinal.get()) {
                        *expression = Expr::Param(position + 1);
                    }
                    ordinal.set(ordinal.get() + 1);
                }
                Ok(())
            },
        )
    }
}

/// What finding the parameter references of a SQL-standard body needs: the body's names are resolved by the same analysis that compiles the body.
struct ParameterRecording<'a> {
    context: &'a RoutineCompilationContext<'a>,
    scope: RoutineParameterScope,
    params: Vec<SQLParam>,
    sites: ParameterSites,
}

impl ParameterRecording<'_> {
    /// Record the parameter references of one statement. A name is a parameter reference exactly when writing the parameter in its place removes one of the references analysis resolves to that parameter by name; a name that resolves to a column leaves them as they were, or makes the statement invalid when the parameter does not type as the column does.
    fn record(&self, statement: &mut Statement) -> Result<bool, SQLError> {
        let candidates = self.sites.collect(statement)?;
        if candidates.is_empty() {
            return Ok(false);
        }
        let original = self.resolved_counts(statement.clone())?;
        let mut chosen = BTreeSet::new();
        for (ordinal, position) in candidates.iter().enumerate() {
            if original[*position] == 0 {
                continue;
            }
            let mut probe = statement.clone();
            self.sites.replace(&mut probe, &BTreeSet::from([ordinal]))?;
            if self
                .resolved_counts(probe)
                .is_ok_and(|counts| counts[*position] < original[*position])
            {
                chosen.insert(ordinal);
            }
        }
        if chosen.is_empty() {
            return Ok(false);
        }
        self.sites.replace(statement, &chosen)?;
        Ok(true)
    }

    /// The number of references to each parameter that analysis of `statement` resolves by name.
    fn resolved_counts(&self, statement: Statement) -> Result<Vec<usize>, SQLError> {
        let mut plan = lower_sql_body_statement(self.context, statement, true, false, false)?;
        let before = parameter_counts(&mut plan, self.params.len());
        let snapshot = self.context.catalog.binding_snapshot()?;
        bind_routine_parameter_references(
            self.context.routines,
            &mut plan,
            &self.params,
            &snapshot.context(),
            &self.scope,
        )?;
        Ok(parameter_counts(&mut plan, self.params.len())
            .into_iter()
            .zip(before)
            .map(|(after, before)| after.saturating_sub(before))
            .collect())
    }
}

/// The number of references to each of `count` parameters in `plan`.
fn parameter_counts(plan: &mut UnifiedPlan, count: usize) -> Vec<usize> {
    let mut counts = vec![0; count];
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let ScalarExpr::Param(index) = expression {
            if let Some(slot) = index.checked_sub(1).and_then(|slot| counts.get_mut(slot)) {
                *slot += 1;
            }
        }
    });
    counts
}
