//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar catalog, session, graph and model dispatch through typed execution services.

use super::{
    graph_lifecycle::GraphLifecycle,
    model_training::{run_deep_learn_projection, ModelTraining},
};
use crate::catalog::{
    context::CatalogContext, security::table_inquiry::TablePrivilegeContext,
    sequence_introspection::SequenceIntrospectionContext,
};
use uqa_core::Value;
use uqa_sql::{
    catalog::{
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
        security::{
            database_inquiry::DatabasePrivilegeInquiry, schema_inquiry::SchemaPrivilegeInquiry,
            sequence_inquiry::SequencePrivilegeInquiry,
        },
    },
    semantics::runtime_scalars::{merge_action_value, no_scalar_arguments, notification_arguments},
    SQLError, ScalarExpr,
};
pub trait ScalarSession {
    fn backend_process_id(&self) -> i32;
    fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError>;
    fn notification_queue_usage(&self) -> Result<f64, SQLError>;
}
pub struct ScalarFunctionContext<'a> {
    pub catalog: CatalogContext<'a>,
    pub sequences: SequenceIntrospectionContext<'a>,
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub database: DatabasePrivilegeInquiry<'a>,
    pub schemas: SchemaPrivilegeInquiry<'a>,
    pub sequence_privileges: SequencePrivilegeInquiry<'a>,
    pub tables: TablePrivilegeContext<'a>,
    pub session: &'a dyn ScalarSession,
    pub graphs: &'a dyn GraphLifecycle,
    pub models: &'a dyn ModelTraining,
}
use crate::query::graph_lifecycle::{
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};
use crate::query::scalar_projection::{run_uqa_highlight, score_projection_value};
use uqa_sql::expr::RowLookup;
use uqa_sql::semantics::scalar_projection::validate_score_projection_args;

pub fn intercept_function(
    context: Option<&ScalarFunctionContext<'_>>,
    name: &str,
    args: &[ScalarExpr],
    row: &dyn RowLookup,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Option<Value>, SQLError> {
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(name);
    if is_catalog_scalar(&lower) {
        let values = args
            .iter()
            .map(evaluate)
            .collect::<Result<Vec<_>, SQLError>>()?;
        return catalog_scalar_value(require_scalar_context(context, &lower)?, &lower, &values)
            .transpose();
    }
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(row, args, evaluate)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, evaluate)?;
            let score = score_projection_value(&lower, args, row)?;
            Ok(Some(score))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(
            require_scalar_context(context, "deep_learn")?.models,
            args,
            evaluate,
        )?)),
        "merge_action" => Ok(Some(merge_action_value(args, row)?)),
        // UQA-native helpers keep their lenient semantics.
        "graph_create" => {
            let graphs = require_scalar_context(context, "graph_create")?.graphs;
            Ok(Some(Value::Bool(run_graph_create_with_evaluator(
                graphs, args, evaluate,
            )?)))
        }
        "graph_drop" => {
            let graphs = require_scalar_context(context, "graph_drop")?.graphs;
            Ok(Some(Value::Bool(run_graph_drop_with_evaluator(
                graphs, args, evaluate,
            )?)))
        }
        // Apache AGE-compatible functions: strict name validation and
        // a void (SQL NULL) return value.
        "create_graph" => Ok(Some(run_age_create_graph_with_evaluator(
            require_scalar_context(context, "create_graph")?.graphs,
            args,
            evaluate,
        )?)),
        "drop_graph" => Ok(Some(run_age_drop_graph_with_evaluator(
            require_scalar_context(context, "drop_graph")?.graphs,
            args,
            evaluate,
        )?)),
        "graph_exists" => Ok(Some(run_age_graph_exists_with_evaluator(
            require_scalar_context(context, "graph_exists")?.graphs,
            args,
            evaluate,
        )?)),
        "create_vlabel" => Ok(Some(run_age_create_vlabel_with_evaluator(
            require_scalar_context(context, "create_vlabel")?.graphs,
            args,
            evaluate,
        )?)),
        "create_elabel" => Ok(Some(run_age_create_elabel_with_evaluator(
            require_scalar_context(context, "create_elabel")?.graphs,
            args,
            evaluate,
        )?)),
        "drop_label" => Ok(Some(run_age_drop_label_with_evaluator(
            require_scalar_context(context, "drop_label")?.graphs,
            args,
            evaluate,
        )?)),
        "alter_graph" => Ok(Some(run_age_alter_graph_with_evaluator(
            require_scalar_context(context, "alter_graph")?.graphs,
            args,
            evaluate,
        )?)),
        _ => Ok(None),
    }
}

fn is_catalog_scalar(name: &str) -> bool {
    matches!(
        name,
        "pg_get_expr"
            | "pg_get_partkeydef"
            | "pg_backend_pid"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "pg_get_serial_sequence"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
            | "pg_sequence_parameters"
            | "pg_get_triggerdef"
            | "pg_get_ruledef"
            | "pg_get_viewdef"
            | "pg_get_indexdef"
            | "format_type"
            | "pg_has_role"
            | "has_database_privilege"
            | "has_schema_privilege"
            | "has_sequence_privilege"
    )
}

fn require_scalar_context<'a, 'services>(
    context: Option<&'a ScalarFunctionContext<'services>>,
    function: &str,
) -> Result<&'a ScalarFunctionContext<'services>, SQLError> {
    context.ok_or_else(|| {
        SQLError::Unsupported(format!("{function} requires an engine-backed projection"))
    })
}
pub fn catalog_scalar_value(
    context: &ScalarFunctionContext<'_>,
    name: &str,
    arguments: &[Value],
) -> Option<Result<Value, SQLError>> {
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(name);
    Some(match lower.as_str() {
        "pg_backend_pid" => no_scalar_arguments(&lower, arguments)
            .map(|()| Value::Int(i64::from(context.session.backend_process_id()))),
        "pg_notify" => (|| {
            let (channel, payload) = notification_arguments(arguments)?;
            context.session.notify(channel, payload)?;
            Ok(Value::Void)
        })(),
        "pg_notification_queue_usage" => no_scalar_arguments(&lower, arguments)
            .and_then(|()| context.session.notification_queue_usage())
            .map(Value::Float),
        "pg_get_expr" => crate::catalog::projection::pg_get_expr_value(&context.catalog, arguments),
        "pg_get_partkeydef" => {
            crate::catalog::projection::pg_get_partkeydef_value(&context.catalog, arguments)
        }
        "pg_get_triggerdef" => {
            crate::catalog::projection::pg_get_triggerdef_value(&context.catalog, arguments)
        }
        "pg_get_ruledef" => {
            crate::catalog::projection::pg_get_ruledef_value(&context.catalog, arguments)
        }
        "pg_get_viewdef" => {
            crate::catalog::projection::pg_get_viewdef_value(&context.catalog, arguments)
        }
        "format_type" => crate::catalog::projection::format_type_value(&context.catalog, arguments),
        "pg_get_indexdef" => {
            crate::catalog::projection::pg_get_indexdef_value(&context.catalog, arguments)
        }
        "pg_get_serial_sequence" => context.sequences.pg_get_serial_sequence_value(arguments),
        "pg_get_sequence_data" => context.sequences.pg_get_sequence_data_value(arguments),
        "pg_sequence_last_value" => context.sequences.pg_sequence_last_value_value(arguments),
        "pg_sequence_parameters" => context.sequences.pg_sequence_parameters_value(arguments),
        "pg_has_role" => uqa_sql::catalog::roles::inquiry::pg_has_role_value(
            context.names,
            context.roles,
            arguments,
        ),
        "has_table_privilege" => context
            .tables
            .inquiry()
            .has_table_privilege_value(arguments),
        "has_column_privilege" => context
            .tables
            .inquiry()
            .has_column_privilege_value(arguments),
        "has_database_privilege" => context.database.has_database_privilege_value(arguments),
        "has_schema_privilege" => context.schemas.has_schema_privilege_value(arguments),
        "has_sequence_privilege" => context
            .sequence_privileges
            .has_sequence_privilege_value(arguments),
        _ => return None,
    })
}

pub fn call_bound_builtin(
    context: &ScalarFunctionContext<'_>,
    binding: &uqa_sql::ast::FunctionBinding,
    arguments: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    if !binding.builtin {
        return None;
    }
    let values = arguments
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    catalog_scalar_value(context, &binding.name, &values)
}

#[cfg(test)]
mod tests;
