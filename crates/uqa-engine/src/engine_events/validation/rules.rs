//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, first_invalid_rule_condition_qualifier, is_boolean_type, rule_action_has_returning,
    rule_action_has_set_operation, validate_rule_action_reference_scopes,
    validate_rule_returning_shape, ColumnType, CreateRule, Engine, Expr, RelationIdentity,
    RelationLookupMode, RelationResolution, RuleConditionNameResolver, RuleEvent,
    RuleRowTypeResolver, SQLError, Statement, StoredViewKind, Value,
};
use crate::engine_events::RuleConditionBinding;
use crate::engine_events::RuleDependencies;

fn rule_condition_has_subquery(condition: &Expr) -> bool {
    condition.any_node(&|node| {
        matches!(
            node,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        )
    })
}

fn rule_condition_row_schema(
    columns: &[(String, ColumnType)],
    binding: &RuleConditionBinding,
) -> uqa_execution::RowSchema {
    let mut names = Vec::with_capacity(columns.len() * 2);
    let mut identities = Vec::with_capacity(columns.len() * 2);
    let mut types = Vec::with_capacity(columns.len() * 2);
    let mut internal = Vec::with_capacity(columns.len() * 2);
    for (side, relation) in [
        ("old", binding.old_relation()),
        ("new", binding.new_relation()),
    ] {
        let Some(relation) = relation else {
            continue;
        };
        for (attribute, (name, ty)) in columns.iter().enumerate() {
            let slot = names.len();
            names.push(name.clone());
            identities.push(uqa_execution::ColumnIdentity::qualified(side, name));
            types.push(Some(ty.clone()));
            internal.push((relation.column(attribute), slot, Some(ty.clone())));
        }
    }
    let schema = uqa_execution::RowSchema::with_identities(names, identities, types);
    uqa_execution::RowSchema::with_physical_internal_aliases(&schema, &internal)
}

fn validate_rule_action_contract(definition: &CreateRule) -> Result<(), SQLError> {
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
    Ok(())
}

impl Engine {
    fn resolve_visible_rule_action_relation(
        &self,
        name: &str,
    ) -> Result<RelationIdentity, SQLError> {
        let Some((canonical, kind)) = self.try_resolve_visible_relation_kind(name)? else {
            return Err(SQLError::UnknownTable(name.to_string()));
        };
        if !matches!(kind, "table" | "view" | "materialized view") {
            return Err(SQLError::UnknownTable(name.to_string()));
        }
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!(
                "decode resolved rule action relation `{canonical}`: {error}"
            ))
        })
    }

    pub(crate) fn resolve_rule_relation(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let RelationResolution::Found(canonical, kind) = self.resolve_bound_relation_kind(name)?
        else {
            return Err(SQLError::UnknownTable(name.to_string()));
        };
        if !matches!(kind, "table" | "view" | "materialized view") {
            return Err(SQLError::UnknownTable(name.to_string()));
        }
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("decode bound rule relation `{canonical}`: {error}"))
        })
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
        stored_plan: Option<&uqa_planner::ExpressionPlan>,
        stored_binding: Option<&RuleConditionBinding>,
    ) -> Result<Option<(uqa_planner::ExpressionPlan, RuleConditionBinding)>, SQLError> {
        let has_subquery = rule_condition_has_subquery(condition);
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
        if !has_subquery && condition.any_node(&|node| matches!(node, Expr::Column(_))) {
            *condition = bind_expr(condition, &mut RuleConditionNameResolver { columns, event })?;
        }
        if let Some(reference) = first_invalid_rule_condition_qualifier(condition) {
            return Err(SQLError::Routine {
                sqlstate: "42P10".into(),
                message: format!("rule WHERE condition cannot refer to relation \"{reference}\""),
            });
        }
        if has_subquery {
            let (mut plan, binding, reused) =
                if let Some((plan, binding)) = stored_plan.zip(stored_binding) {
                    let mut plan = plan.clone();
                    let binding = binding.reallocate_plan_relations(&mut plan);
                    (plan, binding, true)
                } else {
                    let plan = uqa_planner::ExpressionPlan::lower_with(
                        condition.clone(),
                        &|name: &str| self.has_registered_aggregate_function(name),
                    );
                    let column_names = columns
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>();
                    (
                        plan,
                        RuleConditionBinding::for_event(&column_names, event),
                        false,
                    )
                };
            if !reused {
                for subquery in &mut plan.subqueries {
                    self.bind_stored_query_relations(subquery, "CREATE RULE", false)?;
                }
            }
            let schema = rule_condition_row_schema(columns, &binding);
            let ty = crate::sql::bind_catalog_expression_routines_with_outer(
                self,
                &mut plan,
                &[],
                &schema,
            )?;
            if let Some(ty) = ty {
                if !is_boolean_type(&ty) {
                    return Err(SQLError::TypeMismatch(format!(
                        "argument of WHERE must be type boolean, not type {}",
                        ty.sql_name()
                    )));
                }
            }
            crate::sql::reject_stored_regrole_constants(self, condition, None)?;
            return Ok(Some((plan, binding)));
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
        crate::sql::reject_stored_regrole_constants(self, condition, None)?;
        Ok(None)
    }

    fn canonicalize_rule_action_target(
        &self,
        action: &mut Statement,
        lookup_mode: RelationLookupMode,
    ) -> Result<(), SQLError> {
        let (target, target_relation_bound) = match action {
            Statement::Insert(statement) => {
                (&mut statement.table, &mut statement.target_relation_bound)
            }
            Statement::Update(statement) => {
                (&mut statement.table, &mut statement.target_relation_bound)
            }
            Statement::Delete(statement) => {
                (&mut statement.table, &mut statement.target_relation_bound)
            }
            _ => return Ok(()),
        };
        let relation = if lookup_mode == RelationLookupMode::Bound || *target_relation_bound {
            self.resolve_rule_relation(target)?
        } else {
            self.resolve_visible_rule_action_relation(target)?
        };
        *target_relation_bound = true;
        *target = relation.qualified_name();
        Ok(())
    }

    pub(crate) fn rule_action_target_columns(
        &self,
        action: &Statement,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        Ok(self
            .rule_action_target_row_type(action)?
            .into_iter()
            .map(|(column, _)| column)
            .collect())
    }

    fn rule_action_target_row_type(
        &self,
        action: &Statement,
    ) -> Result<Vec<(String, ColumnType)>, SQLError> {
        let table = match action {
            Statement::Insert(statement) => &statement.table,
            Statement::Update(statement) => &statement.table,
            Statement::Delete(statement) => &statement.table,
            _ => return Ok(Vec::new()),
        };
        self.rule_relation_columns(table)
    }

    fn validate_rule_action_definition(
        &self,
        action: &mut Statement,
        event_columns: &[(String, ColumnType)],
        event: RuleEvent,
        lookup_mode: RelationLookupMode,
    ) -> Result<RuleDependencies, SQLError> {
        self.canonicalize_rule_action_target(action, lookup_mode)?;
        let mut dependencies = self.bind_rule_action_relation_dependencies(action, lookup_mode)?;
        let action_row_type = self.rule_action_target_row_type(action)?;
        let action_columns: std::collections::BTreeSet<String> = action_row_type
            .iter()
            .map(|(column, _)| column.clone())
            .collect();
        if lookup_mode == RelationLookupMode::Dynamic {
            *action = crate::engine_events::expand_rule_action_row_stars(
                self,
                action,
                &action_columns,
                event_columns,
                event,
            )?;
            *action =
                crate::engine_events::expand_rule_action_returning_stars(action, &action_row_type);
        }
        dependencies
            .columns
            .extend(self.bind_rule_action_column_dependencies(action)?);
        validate_rule_action_reference_scopes(self, action)?;
        let bound = crate::engine_events::bind_rule_action(
            self,
            action,
            &action_columns,
            &mut RuleRowTypeResolver {
                columns: event_columns,
                event,
            },
        )?;
        let schema = crate::sql::analyze_rule_action_returning_schema(self, bound.clone())?;
        let mut stored_plan = uqa_planner::UnifiedPlan::lower_with(bound, &|name: &str| {
            self.has_registered_aggregate_function(name)
        });
        crate::sql::reject_stored_plan_regrole_constants(self, &mut stored_plan)?;
        let bound_routines = crate::sql::bind_catalog_rule_action_routines(self, &stored_plan)?;
        if let Some(routine_plan) = &bound_routines.query {
            super::super::rule_dependencies::collect_query_routine_dependencies(
                routine_plan,
                &mut dependencies,
            );
        }
        super::super::rule_dependencies::bind_rule_statement_routines(
            action,
            &bound_routines.references,
        )?;
        if let Some(schema) = schema {
            validate_rule_returning_shape(&schema, event_columns)?;
        }
        Ok(dependencies)
    }

    fn bind_rule_condition_object_dependencies(
        &self,
        condition: &mut Expr,
        condition_plan: Option<&uqa_planner::ExpressionPlan>,
        columns: &[(String, ColumnType)],
        event: RuleEvent,
        dependencies: &mut RuleDependencies,
    ) -> Result<(), SQLError> {
        if let Some(plan) = condition_plan {
            super::super::rule_dependencies::collect_expression_routine_dependencies(
                plan,
                dependencies,
            );
            for subquery in &plan.subqueries {
                super::super::rule_dependencies::collect_query_relation_dependencies(
                    subquery,
                    dependencies,
                    &std::collections::BTreeSet::new(),
                )?;
            }
            let routine_references = crate::sql::collect_expression_routine_references(plan)?;
            super::super::rule_dependencies::bind_rule_expr_routines(
                condition,
                &routine_references,
            )?;
            return Ok(());
        }

        let bound = bind_expr(condition, &mut RuleRowTypeResolver { columns, event })?;
        let mut dependency_plan = uqa_planner::ExpressionPlan::lower_with(bound, &|name: &str| {
            self.has_registered_aggregate_function(name)
        });
        crate::sql::bind_catalog_expression_routines_with_outer(
            self,
            &mut dependency_plan,
            &[],
            &uqa_execution::RowSchema::default(),
        )?;
        super::super::rule_dependencies::collect_expression_routine_dependencies(
            &dependency_plan,
            dependencies,
        );
        let routine_references =
            crate::sql::collect_expression_routine_references(&dependency_plan)?;
        super::super::rule_dependencies::bind_rule_expr_routines(condition, &routine_references)
    }

    pub(in crate::engine_events) fn validate_rule_definition(
        &self,
        definition: &mut CreateRule,
        lookup_mode: RelationLookupMode,
        stored_condition_plan: Option<&uqa_planner::ExpressionPlan>,
        stored_condition_binding: Option<&RuleConditionBinding>,
    ) -> Result<
        (
            RelationIdentity,
            Option<uqa_planner::ExpressionPlan>,
            Option<RuleConditionBinding>,
            RuleDependencies,
        ),
        SQLError,
    > {
        let (relation, _) = self.resolve_event_relation_kind(&definition.table, lookup_mode)?;
        definition.table = relation.qualified_name();
        if lookup_mode == RelationLookupMode::Dynamic {
            self.ensure_event_relation_owner(&relation, None)?;
        }
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
        let mut dependencies = RuleDependencies::default();
        if let Some(condition) = &mut definition.condition {
            let condition_dependencies =
                self.bind_rule_condition_relation_dependencies(condition, lookup_mode)?;
            dependencies
                .relations
                .extend(condition_dependencies.relations);
            dependencies
                .columns
                .extend(self.bind_rule_condition_column_dependencies(condition)?);
        }
        let condition = definition
            .condition
            .as_mut()
            .map(|condition| {
                self.validate_rule_condition(
                    condition,
                    &columns,
                    definition.event,
                    stored_condition_plan,
                    stored_condition_binding,
                )
            })
            .transpose()?
            .flatten();
        let (condition_plan, condition_binding) = condition.map_or_else(
            || (None, None),
            |(plan, binding)| (Some(plan), Some(binding)),
        );
        if let Some(condition) = &mut definition.condition {
            self.bind_rule_condition_object_dependencies(
                condition,
                condition_plan.as_ref(),
                &columns,
                definition.event,
                &mut dependencies,
            )?;
            dependencies.columns.extend(
                crate::engine_events::rule_expr_row_columns(condition)
                    .into_iter()
                    .map(|column| crate::engine_events::RuleColumnDependency {
                        relation: relation.clone(),
                        column,
                    }),
            );
        }
        validate_rule_action_contract(definition)?;
        for action in &mut definition.actions {
            let action_dependencies = self.validate_rule_action_definition(
                action,
                &columns,
                definition.event,
                lookup_mode,
            )?;
            dependencies.relations.extend(action_dependencies.relations);
            dependencies.columns.extend(action_dependencies.columns);
            dependencies.routines.extend(action_dependencies.routines);
            let action_columns = self.rule_action_target_columns(action)?;
            dependencies.columns.extend(
                crate::engine_events::rule_statement_row_columns(self, action, &action_columns)?
                    .into_iter()
                    .map(|column| crate::engine_events::RuleColumnDependency {
                        relation: relation.clone(),
                        column,
                    }),
            );
        }
        super::super::synchronize_rule_sql_text(definition)?;
        Ok((relation, condition_plan, condition_binding, dependencies))
    }
}
