//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-function resolution and definition validation.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, CreateTrigger, Expr, FunctionReturns, TriggerEvent, TriggerTiming};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use crate::engine_user_functions::{
    canonical_routine_type_name, routine_signature_types, CompiledFunctionBody, SQLUserFunction,
};
use crate::{Arc, Engine, RelationIdentity};

struct TriggerConditionTypeResolver<'a> {
    columns: &'a [uqa_sql::ast::ColumnDef],
}

impl VariableResolver for TriggerConditionTypeResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            return Ok(None);
        }
        Ok(self
            .columns
            .iter()
            .find(|definition| definition.name == column)
            .map(|definition| ResolvedVariable {
                value: Value::Null,
                declared_type: Some(definition.ty.sql_name()),
            }))
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn is_boolean_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Boolean => true,
        ColumnType::Domain { base, .. } => is_boolean_type(base),
        _ => false,
    }
}

fn validate_trigger_condition_references(
    definition: &CreateTrigger,
    columns: &[uqa_sql::ast::ColumnDef],
    condition: &Expr,
) -> Result<(), SQLError> {
    if condition.any_node(&|node| {
        matches!(
            node,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        )
    }) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot use subquery in trigger WHEN condition".into(),
        });
    }
    if condition.any_node(&|node| matches!(node, Expr::Column(_))) {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: "trigger WHEN condition must qualify row columns with OLD or NEW".into(),
        });
    }
    let references_old = condition.any_node(&|node| {
        matches!(node, Expr::QualifiedColumn { qualifier, .. } if qualifier.eq_ignore_ascii_case("old"))
    });
    let references_new = condition.any_node(&|node| {
        matches!(node, Expr::QualifiedColumn { qualifier, .. } if qualifier.eq_ignore_ascii_case("new"))
    });
    if !definition.row && (references_old || references_new) {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: "statement trigger's WHEN condition cannot reference row values".into(),
        });
    }
    if references_old && definition.events.contains(&TriggerEvent::Insert) {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "INSERT trigger's WHEN condition cannot reference OLD values".into(),
        });
    }
    if references_new && definition.events.contains(&TriggerEvent::Delete) {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "DELETE trigger's WHEN condition cannot reference NEW values".into(),
        });
    }
    let invalid_qualified_reference = std::cell::RefCell::new(None);
    let _ = condition.any_node(&|node| {
        let Expr::QualifiedColumn { qualifier, column } = node else {
            return false;
        };
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            *invalid_qualified_reference.borrow_mut() = Some(format!("{qualifier}.{column}"));
            return true;
        }
        if !columns.iter().any(|definition| definition.name == *column) {
            *invalid_qualified_reference.borrow_mut() = Some(column.clone());
            return true;
        }
        false
    });
    if let Some(reference) = invalid_qualified_reference.into_inner() {
        return Err(SQLError::UnknownColumn(reference));
    }
    if definition.timing == TriggerTiming::Before && references_new {
        let generated = columns
            .iter()
            .filter(|column| column.generated.is_some())
            .map(|column| column.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if condition.any_node(&|node| {
            matches!(
                node,
                Expr::QualifiedColumn { qualifier, column }
                    if qualifier.eq_ignore_ascii_case("new")
                        && generated.contains(column.as_str())
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "BEFORE trigger's WHEN condition cannot reference NEW generated columns"
                    .into(),
            });
        }
    }
    Ok(())
}

impl Engine {
    pub(super) fn resolve_trigger_table(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let canonical = self
            .try_resolve_table_name(name)
            .map_err(|error| {
                SQLError::Internal(format!("resolve trigger relation `{name}`: {error}"))
            })?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{canonical}`: {error}"))
        })
    }

    pub(crate) fn resolve_trigger_function(
        &self,
        name: &str,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let candidates = self
            .lookup_sql_functions(name)
            .unwrap_or_default()
            .into_iter()
            .filter(|function| {
                !function.def.is_procedure && routine_signature_types(&function.def).is_empty()
            })
            .collect::<Vec<_>>();
        let function = match candidates.as_slice() {
            [function] => function.clone(),
            [] => {
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: format!("function {name}() does not exist"),
                })
            }
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42725".into(),
                    message: format!("function name \"{name}\" is not unique"),
                })
            }
        };
        let returns_trigger = matches!(
            &function.def.returns,
            FunctionReturns::Scalar { type_name }
                if canonical_routine_type_name(type_name) == "trigger"
        );
        if !returns_trigger {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!("function {} must return type trigger", function.def.name),
            });
        }
        if !matches!(function.compiled, CompiledFunctionBody::PLpgSQL(_)) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "only LANGUAGE plpgsql trigger functions are executable".into(),
            });
        }
        Ok(function)
    }

    pub(super) fn validate_trigger_definition(
        &self,
        definition: &mut CreateTrigger,
    ) -> Result<RelationIdentity, SQLError> {
        let relation = self.resolve_trigger_table(&definition.table)?;
        definition.table = relation.qualified_name();
        definition.function.clone_from(
            &self
                .resolve_trigger_function(&definition.function)?
                .def
                .name,
        );
        let columns = self
            .try_describe_table(&definition.table)
            .map_err(|error| SQLError::Internal(format!("read trigger columns: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(definition.table.clone()))?;
        if definition.events.contains(&TriggerEvent::Truncate) && definition.row {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "TRUNCATE FOR EACH ROW triggers are not supported".into(),
            });
        }
        if !definition.update_columns.is_empty()
            && !definition.events.contains(&TriggerEvent::Update)
        {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "UPDATE OF columns may only be specified for an UPDATE trigger".into(),
            });
        }
        let mut seen_update_columns = std::collections::BTreeSet::new();
        for column in &definition.update_columns {
            if !columns.iter().any(|definition| definition.name == *column) {
                return Err(SQLError::UnknownColumn(format!(
                    "{}.{column}",
                    definition.table
                )));
            }
            if !seen_update_columns.insert(column) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!("column \"{column}\" specified more than once"),
                });
            }
        }
        if let Some(mut condition) = definition.when.take() {
            self.validate_trigger_condition(definition, &columns, &mut condition)?;
            definition.when = Some(condition);
        }
        Ok(relation)
    }

    fn validate_trigger_condition(
        &self,
        definition: &CreateTrigger,
        columns: &[uqa_sql::ast::ColumnDef],
        condition: &mut Expr,
    ) -> Result<(), SQLError> {
        validate_trigger_condition_references(definition, columns, condition)?;
        let bound = bind_expr(condition, &mut TriggerConditionTypeResolver { columns })?;
        let lowered = uqa_planner::ExpressionPlan::lower(bound);
        match uqa_execution::common_context_expression_type(
            &lowered.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            Some(self),
        )? {
            Some(ty) if !is_boolean_type(&ty) => {
                return Err(SQLError::TypeMismatch(format!(
                    "argument of WHEN must be type boolean, not type {}",
                    ty.sql_name()
                )))
            }
            None => {
                if let Expr::Literal(value @ (Value::Str(_) | Value::FixedChar(_))) = condition {
                    *value = uqa_sql::expr::cast_value(value, "boolean")?;
                } else {
                    *condition = Expr::Cast {
                        expr: Box::new(condition.clone()),
                        ty: "boolean".into(),
                    };
                }
            }
            Some(_) => {}
        }
        Ok(())
    }
}
