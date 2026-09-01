//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, first_invalid_rule_condition_qualifier, is_boolean_type, rule_action_has_returning,
    rule_action_has_set_operation, validate_rule_action_reference_scopes,
    validate_rule_returning_shape, ColumnType, CreateRule, Engine, Expr, RelationIdentity,
    RuleConditionNameResolver, RuleEvent, RuleRowTypeResolver, SQLError, Statement, StoredViewKind,
    Value,
};

impl Engine {
    pub(crate) fn resolve_rule_relation(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let candidates = self.relation_lookup_candidates(name).map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{name}`: {error}"))
        })?;
        let tables = self.storage.tables.read();
        let views = self.durable.views.read();
        candidates
            .into_iter()
            .find(|relation| tables.contains_key(relation) || views.contains_key(relation))
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }

    pub(crate) fn rule_relation_columns(
        &self,
        name: &str,
    ) -> Result<Vec<(String, ColumnType)>, SQLError> {
        if let Some(columns) = self
            .try_describe_table_row_type(name)
            .map_err(|error| SQLError::Internal(format!("read rule columns: {error}")))?
        {
            return Ok(columns
                .into_iter()
                .map(|column| (column.name, column.ty))
                .collect());
        }
        let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("decode rule relation `{name}`: {error}"))
        })?;
        let view = self
            .durable
            .views
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        let schema = self.stored_view_schema(&view)?;
        Ok(schema
            .columns()
            .iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    schema.public_name(index).unwrap_or(name).to_string(),
                    schema
                        .column_type(index)
                        .cloned()
                        .unwrap_or(ColumnType::Text),
                )
            })
            .collect())
    }

    fn validate_select_rule_contract(
        definition: &CreateRule,
        is_view: bool,
    ) -> Result<(), SQLError> {
        if definition.event != RuleEvent::Select && definition.name == "_RETURN" {
            let relation =
                RelationIdentity::from_legacy_name(&definition.table).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode rule relation `{}`: {error}",
                        definition.table
                    ))
                })?;
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "non-view rule for \"{}\" must not be named \"_RETURN\"",
                    relation.name
                ),
            });
        }
        if definition.event == RuleEvent::Select {
            if !is_view {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "relation \"{}\" cannot have ON SELECT rules",
                        definition.table
                    ),
                });
            }
            if definition.name != "_RETURN"
                || !definition.instead
                || definition.condition.is_some()
                || !matches!(definition.actions.as_slice(), [Statement::Select(_)])
            {
                return Err(SQLError::Routine {
                    sqlstate: "42P17".into(),
                    message: "view rule must be named \"_RETURN\", unconditional, INSTEAD, and have one SELECT action".into(),
                });
            }
        }
        Ok(())
    }

    fn validate_rule_condition(
        &self,
        condition: &mut Expr,
        columns: &[(String, ColumnType)],
        event: RuleEvent,
    ) -> Result<(), SQLError> {
        if condition.any_node(&|node| {
            matches!(
                node,
                Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot use subquery in rule WHERE condition".into(),
            });
        }
        if condition.contains_aggregate()
            || condition.contains_window()
            || condition.any_node(&|node| {
                matches!(node, Expr::Func { name, .. } if self.has_registered_aggregate_function(name))
            })
        {
            return Err(SQLError::Routine {
                sqlstate: "42803".into(),
                message: "aggregate and window functions are not allowed in rule WHERE conditions"
                    .into(),
            });
        }
        if condition.any_node(&|node| matches!(node, Expr::Column(_))) {
            *condition = bind_expr(condition, &mut RuleConditionNameResolver { columns, event })?;
        }
        if let Some(reference) = first_invalid_rule_condition_qualifier(condition) {
            return Err(SQLError::Routine {
                sqlstate: "42P10".into(),
                message: format!("rule WHERE condition cannot refer to relation \"{reference}\""),
            });
        }
        let bound = bind_expr(condition, &mut RuleRowTypeResolver { columns, event })?;
        let lowered = uqa_planner::ExpressionPlan::lower(bound);
        match uqa_execution::common_context_expression_type(
            &lowered.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            Some(self),
        )? {
            Some(ty) if !is_boolean_type(&ty) => {
                return Err(SQLError::TypeMismatch(format!(
                    "argument of WHERE must be type boolean, not type {}",
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

    fn canonicalize_rule_action_target(&self, action: &mut Statement) -> Result<(), SQLError> {
        let target = match action {
            Statement::Insert(statement) => &mut statement.table,
            Statement::Update(statement) => &mut statement.table,
            Statement::Delete(statement) => &mut statement.table,
            _ => return Ok(()),
        };
        *target = self.resolve_rule_relation(target)?.qualified_name();
        Ok(())
    }

    pub(crate) fn rule_action_target_columns(
        &self,
        action: &Statement,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        let table = match action {
            Statement::Insert(statement) => &statement.table,
            Statement::Update(statement) => &statement.table,
            Statement::Delete(statement) => &statement.table,
            _ => return Ok(std::collections::BTreeSet::new()),
        };
        Ok(self
            .rule_relation_columns(table)?
            .into_iter()
            .map(|(column, _)| column)
            .collect())
    }

    pub(in crate::engine_events) fn validate_rule_definition(
        &self,
        definition: &mut CreateRule,
    ) -> Result<RelationIdentity, SQLError> {
        let relation = self.resolve_rule_relation(&definition.table)?;
        definition.table = relation.qualified_name();
        let stored_view_kind = self
            .durable
            .views
            .read()
            .get(&relation)
            .map(|view| view.kind);
        if stored_view_kind == Some(StoredViewKind::Materialized) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "rules on materialized views are not supported".into(),
            });
        }
        let is_view = stored_view_kind == Some(StoredViewKind::View);
        Self::validate_select_rule_contract(definition, is_view)?;
        let columns = self.rule_relation_columns(&definition.table)?;
        if let Some(condition) = definition.condition.as_mut() {
            self.validate_rule_condition(condition, &columns, definition.event)?;
        }
        if definition.condition.is_some()
            && definition.actions.iter().any(rule_action_has_set_operation)
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "conditional UNION/INTERSECT/EXCEPT statements are not implemented".into(),
            });
        }
        let returning_actions = definition
            .actions
            .iter()
            .filter(|action| rule_action_has_returning(action))
            .count();
        if returning_actions > 1 {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot have multiple RETURNING lists in a rule".into(),
            });
        }
        if returning_actions != 0 && definition.condition.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "RETURNING lists are not supported in conditional rules".into(),
            });
        }
        if returning_actions != 0 && !definition.instead {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "RETURNING lists are not supported in non-INSTEAD rules".into(),
            });
        }
        for action in &mut definition.actions {
            self.canonicalize_rule_action_target(action)?;
            validate_rule_action_reference_scopes(action)?;
            let action_columns = self.rule_action_target_columns(action)?;
            let bound = crate::engine_events::bind_rule_action(
                action,
                &action_columns,
                &mut RuleRowTypeResolver {
                    columns: &columns,
                    event: definition.event,
                },
            )?;
            if let Some(schema) = crate::sql::analyze_rule_action_returning_schema(self, bound)? {
                validate_rule_returning_shape(&schema, &columns)?;
            }
        }
        Ok(relation)
    }
}
