//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, canonical_routine_type_name, is_boolean_type, routine_signature_types,
    validate_trigger_condition_references, validate_trigger_transition_relation, Arc, ColumnDef,
    ColumnType, CompiledFunctionBody, CreateTrigger, Engine, Expr, FunctionReturns,
    RelationIdentity, RelationLookupMode, RelationResolution, SQLError, SQLUserFunction,
    TriggerConditionTypeResolver, TriggerEvent, TriggerTiming, Value,
};

impl Engine {
    pub(in crate::engine_events) fn resolve_trigger_table(
        &self,
        name: &str,
    ) -> Result<RelationIdentity, SQLError> {
        let RelationResolution::Found(canonical, kind) = self.resolve_bound_relation_kind(name)?
        else {
            return Err(SQLError::UnknownTable(name.to_string()));
        };
        if !matches!(
            kind,
            "table" | "view" | "materialized view" | "foreign table"
        ) {
            return Err(SQLError::UnknownTable(name.to_string()));
        }
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!(
                "decode bound trigger relation `{canonical}`: {error}"
            ))
        })
    }

    fn trigger_relation_columns(
        &self,
        relation: &RelationIdentity,
        kind: &str,
    ) -> Result<Vec<ColumnDef>, SQLError> {
        if kind == "table" {
            return self
                .try_describe_table_row_type(&relation.qualified_name())
                .map_err(|error| SQLError::Internal(format!("read trigger columns: {error}")))?
                .ok_or_else(|| SQLError::UnknownTable(relation.qualified_name()));
        }
        if kind == "foreign table" {
            let table = self
                .durable
                .foreign_tables
                .read()
                .get(relation)
                .cloned()
                .ok_or_else(|| SQLError::UnknownTable(relation.qualified_name()))?;
            return Ok(table
                .columns
                .into_iter()
                .map(|column| trigger_column(column.name, column.ty))
                .collect());
        }
        let view = self
            .restored_catalog_view_definition(&relation.qualified_name())?
            .ok_or_else(|| SQLError::UnknownTable(relation.qualified_name()))?;
        let schema = self.stored_view_schema(&view)?;
        Ok(schema
            .columns()
            .iter()
            .enumerate()
            .map(|(position, name)| {
                trigger_column(
                    schema.public_name(position).unwrap_or(name).to_string(),
                    schema
                        .column_type(position)
                        .cloned()
                        .unwrap_or(ColumnType::Text),
                )
            })
            .collect())
    }

    pub(in crate::engine_events) fn trigger_relation_from_resolution(
        name: &str,
        resolution: RelationResolution,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        match resolution {
            RelationResolution::Found(canonical, "foreign table") => {
                let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode resolved trigger relation `{canonical}`: {error}"
                    ))
                })?;
                Ok((relation, "foreign table"))
            }
            resolution => Self::event_relation_from_resolution(name, resolution),
        }
    }

    pub(in crate::engine_events) fn resolve_trigger_relation_kind(
        &self,
        name: &str,
        lookup_mode: RelationLookupMode,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let resolution = match lookup_mode {
            RelationLookupMode::Dynamic => self.resolve_visible_relation_kind(name)?,
            RelationLookupMode::Bound => self.resolve_bound_relation_kind(name)?,
        };
        Self::trigger_relation_from_resolution(name, resolution)
    }

    fn resolve_trigger_function_candidate(
        &self,
        name: &str,
        lookup_mode: RelationLookupMode,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let candidates = match lookup_mode {
            RelationLookupMode::Dynamic => self.lookup_visible_sql_functions(name)?,
            RelationLookupMode::Bound => self.lookup_bound_sql_functions(name),
        }
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
        Ok(function)
    }

    fn validate_trigger_function(function: &SQLUserFunction) -> Result<(), SQLError> {
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
        Ok(())
    }

    pub(crate) fn resolve_trigger_function(
        &self,
        name: &str,
        lookup_mode: RelationLookupMode,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let function = self.resolve_trigger_function_candidate(name, lookup_mode)?;
        Self::validate_trigger_function(&function)?;
        Ok(function)
    }

    pub(crate) fn resolve_bound_trigger_function(
        &self,
        name: &str,
        object_id: Option<[u8; 16]>,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let Some(object_id) = object_id else {
            return self.resolve_trigger_function(name, RelationLookupMode::Bound);
        };
        let binding = uqa_sql::ast::FunctionBinding {
            object_id: Some(object_id),
            name: name.to_string(),
            argument_types: Vec::new(),
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        };
        let candidates = self
            .lookup_bound_sql_functions_by_binding(&binding)
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
                return Err(SQLError::Internal(format!(
                    "routine object identity for trigger function `{name}` is not unique"
                )))
            }
        };
        Self::validate_trigger_function(&function)?;
        Ok(function)
    }

    fn ensure_trigger_creation_privilege(
        &self,
        relation: &RelationIdentity,
        relation_kind: &str,
    ) -> Result<(), SQLError> {
        let canonical = relation.qualified_name();
        match relation_kind {
            "table" => self.ensure_table_privilege(
                &canonical,
                crate::engine_table_security::TableAclPrivilege::Trigger,
            ),
            "view" => {
                let view = self
                    .restored_catalog_view_definition(&canonical)?
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "resolved trigger view `{canonical}` has no catalog definition"
                        ))
                    })?;
                self.ensure_view_privilege_for(
                    &canonical,
                    &view,
                    &self.current_user_name(),
                    crate::engine_table_security::TableAclPrivilege::Trigger,
                )
            }
            "foreign table" => self.ensure_foreign_table_privilege(
                &canonical,
                crate::engine_table_security::TableAclPrivilege::Trigger,
            ),
            _ => Ok(()),
        }
    }

    fn validate_trigger_relation_kind(
        definition: &CreateTrigger,
        relation: &RelationIdentity,
        relation_kind: &str,
    ) -> Result<(), SQLError> {
        match relation_kind {
            "table" if definition.timing == TriggerTiming::InsteadOf => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{}\" is a table", relation.name),
            }),
            "view" if definition.timing == TriggerTiming::InsteadOf => {
                if definition.events.contains(&TriggerEvent::Truncate)
                    || !definition.transition_relations.is_empty()
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: format!("\"{}\" is a view", relation.name),
                    });
                }
                if !definition.row {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "INSTEAD OF triggers must be FOR EACH ROW".into(),
                    });
                }
                if definition.when.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "INSTEAD OF triggers cannot have WHEN conditions".into(),
                    });
                }
                if !definition.update_columns.is_empty() {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "INSTEAD OF triggers cannot have column lists".into(),
                    });
                }
                Ok(())
            }
            "view"
                if definition.events.contains(&TriggerEvent::Truncate)
                    || !definition.transition_relations.is_empty()
                    || definition.row =>
            {
                Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is a view", relation.name),
                })
            }
            "materialized view" => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("relation \"{}\" cannot have triggers", relation.name),
            }),
            "foreign table"
                if definition.constraint || definition.timing == TriggerTiming::InsteadOf =>
            {
                Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is a foreign table", relation.name),
                })
            }
            "view" | "foreign table" => Ok(()),
            kind if kind != "table" => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("relation \"{}\" cannot have triggers", relation.name),
            }),
            _ => Ok(()),
        }
    }

    pub(in crate::engine_events) fn validate_trigger_definition(
        &self,
        definition: &mut CreateTrigger,
        lookup_mode: RelationLookupMode,
    ) -> Result<(RelationIdentity, bool), SQLError> {
        if definition.constraint && definition.or_replace {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "CREATE OR REPLACE CONSTRAINT TRIGGER is not supported".into(),
            });
        }
        if definition.constraint && (!definition.row || definition.timing != TriggerTiming::After) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "constraint triggers must be AFTER ROW triggers".into(),
            });
        }
        if !definition.constraint
            && (definition.deferrability.is_deferrable() || definition.referenced_table.is_some())
        {
            return Err(SQLError::Internal(
                "ordinary trigger retained constraint-only metadata".into(),
            ));
        }
        let (relation, relation_kind) =
            self.resolve_trigger_relation_kind(&definition.table, lookup_mode)?;
        definition.table = relation.qualified_name();
        Self::validate_trigger_relation_kind(definition, &relation, relation_kind)?;
        if lookup_mode == RelationLookupMode::Dynamic {
            self.ensure_trigger_creation_privilege(&relation, relation_kind)?;
        }
        if relation_kind == "foreign table" && !definition.transition_relations.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{}\" is a foreign table", relation.name),
            });
        }
        if let Some(referenced_table) = definition.referenced_table.as_mut() {
            let (referenced, referenced_kind) =
                self.resolve_event_relation_kind(referenced_table, lookup_mode)?;
            if referenced_kind != "table" {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is a {referenced_kind}", referenced.name),
                });
            }
            *referenced_table = referenced.qualified_name();
        }
        let requested_function = definition.function.clone();
        let function = self.resolve_trigger_function_candidate(&requested_function, lookup_mode)?;
        if lookup_mode == RelationLookupMode::Dynamic {
            self.ensure_routine_execute_privilege_named(&function.def, &requested_function)?;
        }
        Self::validate_trigger_function(&function)?;
        definition.function.clone_from(&function.def.name);
        let columns = self.trigger_relation_columns(&relation, relation_kind)?;
        if relation_kind == "table" {
            self.validate_trigger_transition_relations(definition, &relation)?;
        }
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
        let mut condition_routine_bindings_changed = false;
        if let Some(mut condition) = definition.when.take() {
            condition_routine_bindings_changed =
                self.validate_trigger_condition(definition, &columns, &mut condition)?;
            definition.when = Some(condition);
        }
        Ok((relation, condition_routine_bindings_changed))
    }

    fn validate_trigger_transition_relations(
        &self,
        definition: &CreateTrigger,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        if definition.transition_relations.is_empty() {
            return Ok(());
        }
        let hierarchy = self.loaded_table_hierarchy(relation).ok_or_else(|| {
            SQLError::Internal(format!(
                "trigger table `{}` disappeared during validation",
                definition.table
            ))
        })?;
        if definition.row && hierarchy.partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("\"{}\" is a partitioned table", relation.name),
            });
        }
        let mut old_table = None;
        let mut new_table = None;
        for transition in &definition.transition_relations {
            validate_trigger_transition_relation(definition, &hierarchy, transition)?;
            let duplicate = if transition.is_new {
                new_table.replace(transition.name.as_str())
            } else {
                old_table.replace(transition.name.as_str())
            };
            if duplicate.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "42P17".into(),
                    message: format!(
                        "{} TABLE cannot be specified multiple times",
                        if transition.is_new { "NEW" } else { "OLD" }
                    ),
                });
            }
        }
        if old_table.is_some() && old_table == new_table {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "OLD TABLE name and NEW TABLE name cannot be the same".into(),
            });
        }
        Ok(())
    }

    fn validate_trigger_condition(
        &self,
        definition: &CreateTrigger,
        columns: &[uqa_sql::ast::ColumnDef],
        condition: &mut Expr,
    ) -> Result<bool, SQLError> {
        validate_trigger_condition_references(definition, columns, condition)?;
        let bound = bind_expr(condition, &mut TriggerConditionTypeResolver { columns })?;
        let mut plan = uqa_planner::ExpressionPlan::lower_with(bound, &|name: &str| {
            self.has_registered_aggregate_function(name)
        });
        let ty = crate::sql::bind_catalog_expression_routines_with_outer(
            self,
            &mut plan,
            &[],
            &uqa_execution::RowSchema::default(),
        )?;
        if !ty.as_ref().is_some_and(is_boolean_type) {
            if let Expr::Literal(value @ (Value::Str(_) | Value::FixedChar(_))) = condition {
                *value = uqa_sql::expr::cast_value(value, "boolean")?;
            } else if let Some(ty) = ty {
                return Err(SQLError::TypeMismatch(format!(
                    "argument of WHEN must be type boolean, not type {}",
                    ty.sql_name()
                )));
            } else {
                *condition = Expr::Cast {
                    expr: Box::new(condition.clone()),
                    ty: "boolean".into(),
                };
            }
        }
        crate::sql::reject_stored_regrole_constants(self, condition, None)?;
        let references = crate::sql::collect_expression_routine_references(&plan)?;
        super::super::bind_stored_expression_routines(condition, &references)
    }
}

fn trigger_column(name: String, ty: ColumnType) -> ColumnDef {
    ColumnDef {
        name,
        ty,
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    }
}
