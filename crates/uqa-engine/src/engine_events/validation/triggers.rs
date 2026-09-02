//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, canonical_routine_type_name, is_boolean_type, routine_signature_types,
    validate_trigger_condition_references, validate_trigger_transition_relation, Arc, ColumnDef,
    ColumnType, CompiledFunctionBody, CreateTrigger, Engine, Expr, FunctionReturns,
    RelationIdentity, SQLError, SQLUserFunction, StoredViewKind, TriggerConditionTypeResolver,
    TriggerEvent, TriggerTiming, Value,
};

impl Engine {
    fn resolve_trigger_relation_kind(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let candidates = self.relation_lookup_candidates(name).map_err(|error| {
            SQLError::Internal(format!("resolve trigger relation `{name}`: {error}"))
        })?;
        let tables = self.storage.tables.read();
        let views = self.durable.views.read();
        candidates
            .into_iter()
            .find_map(|relation| {
                if tables.contains_key(&relation) {
                    return Some((relation, "table"));
                }
                views.get(&relation).map(|view| {
                    (
                        relation,
                        match view.kind {
                            StoredViewKind::View => "view",
                            StoredViewKind::Materialized => "materialized view",
                        },
                    )
                })
            })
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }

    pub(in crate::engine_events) fn resolve_trigger_table(
        &self,
        name: &str,
    ) -> Result<RelationIdentity, SQLError> {
        self.resolve_trigger_relation_kind(name)
            .map(|(relation, _)| relation)
    }

    fn trigger_relation_columns(
        &self,
        relation: &RelationIdentity,
        kind: &str,
    ) -> Result<Vec<ColumnDef>, SQLError> {
        if kind == "table" {
            return self
                .try_describe_table(&relation.qualified_name())
                .map_err(|error| SQLError::Internal(format!("read trigger columns: {error}")))?
                .ok_or_else(|| SQLError::UnknownTable(relation.qualified_name()));
        }
        let view = self
            .restored_catalog_view_definition(&relation.qualified_name())?
            .ok_or_else(|| SQLError::UnknownTable(relation.qualified_name()))?;
        let schema = self.stored_view_schema(&view)?;
        Ok(schema
            .columns()
            .iter()
            .enumerate()
            .map(|(position, name)| ColumnDef {
                name: schema.public_name(position).unwrap_or(name).to_string(),
                ty: schema
                    .column_type(position)
                    .cloned()
                    .unwrap_or(ColumnType::Text),
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
            })
            .collect())
    }

    pub(crate) fn resolve_trigger_function(
        &self,
        name: &str,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let candidates = self
            .lookup_visible_sql_functions(name)?
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
            "view" => Ok(()),
            "materialized view" => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("relation \"{}\" cannot have triggers", relation.name),
            }),
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
    ) -> Result<RelationIdentity, SQLError> {
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
        let (relation, relation_kind) = self.resolve_trigger_relation_kind(&definition.table)?;
        definition.table = relation.qualified_name();
        Self::validate_trigger_relation_kind(definition, &relation, relation_kind)?;
        if let Some(referenced_table) = definition.referenced_table.as_mut() {
            let (referenced, referenced_kind) =
                self.resolve_trigger_relation_kind(referenced_table)?;
            if referenced_kind != "table" {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is a {referenced_kind}", referenced.name),
                });
            }
            *referenced_table = referenced.qualified_name();
        }
        definition.function.clone_from(
            &self
                .resolve_trigger_function(&definition.function)?
                .def
                .name,
        );
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
        if let Some(mut condition) = definition.when.take() {
            self.validate_trigger_condition(definition, &columns, &mut condition)?;
            definition.when = Some(condition);
        }
        Ok(relation)
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
        crate::sql::reject_stored_regrole_constants(self, condition, None)
    }
}
