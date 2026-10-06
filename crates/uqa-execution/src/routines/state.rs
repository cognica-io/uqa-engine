//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpreter activation state, expression binding, and routine lifecycle.

use super::{
    cast_value_from, coercion_type_name, BTreeSet, ColumnType, CreateFunction, DatumResolver, Flow,
    FunctionReturns, HashMap, Interpreter, PLpgSQLBlock, PLpgSQLExpression, PLpgSQLFunction,
    PLpgSQLStatement, RoutineContext, RoutineOutcome, SQLError, SQLParam, SQLResult, Statement,
    Value,
};
use uqa_sql::binding::{resolve_variable_sites, VariableSiteResolution};
use uqa_sql::plan::UnifiedPlan;
use uqa_sql::ScalarExpr;

impl<'a> Interpreter<'a> {
    pub fn new(
        services: RoutineContext<'a>,
        def: &'a CreateFunction,
        parsed: &'a PLpgSQLFunction,
        bound: Vec<Value>,
    ) -> Result<Self, SQLError> {
        services.statements.load_language_library("plpgsql");
        let datums = &parsed.datums;
        if datums.len() < def.params.len() {
            return Err(SQLError::Internal(
                "PL/pgSQL datum table is smaller than the parameter list".into(),
            ));
        }
        let signature_arity = def.signature_arity();
        if bound.len() != signature_arity {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL routine `{}` received {} bound arguments for a signature of {signature_arity}",
                def.name,
                bound.len()
            )));
        }
        let loop_vars: BTreeSet<usize> = parsed.loop_local_variable_datums();
        let cursor_arguments: BTreeSet<usize> = parsed.cursor_argument_datums();
        let block_variables = parsed.block_variable_datums();
        let mut bindings: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, datum) in datums.iter().enumerate() {
            if loop_vars.contains(&idx)
                || cursor_arguments.contains(&idx)
                || block_variables.contains(&idx)
            {
                continue;
            }
            if let Some(name) = datum.name() {
                if !name.is_empty() {
                    bindings.entry(name.to_string()).or_default().push(idx);
                }
            }
        }
        let mut out_datums = Vec::new();
        for (idx, param) in def.params.iter().enumerate() {
            if matches!(
                param.mode,
                uqa_sql::ast::FunctionParamMode::Out
                    | uqa_sql::ast::FunctionParamMode::InOut
                    | uqa_sql::ast::FunctionParamMode::Table
            ) {
                out_datums.push(idx);
            }
        }
        let mut interpreter = Self {
            preparations: services.statements.plpgsql_preparations(def, parsed),
            services,
            def,
            datums,
            values: vec![Value::Null; datums.len()],
            record_types: HashMap::new(),
            bindings,
            cursor_arguments,
            err_stack: Vec::new(),
            set_rows: Vec::new(),
            ret: Value::Null,
            ret_record_types: None,
            out_datums,
            found: parsed.found_datum,
            last_row_count: 0,
            is_set: def.returns_set(),
            variable_conflict: parsed.variable_conflict,
        };
        // Bind call arguments onto the leading parameter datums.
        // Procedure OUT arguments start NULL (the placeholder value a
        // caller passes is discarded, matching PostgreSQL 14+).
        let mut bound = bound.into_iter();
        for (idx, param) in def.params.iter().enumerate() {
            let takes_argument = match param.mode {
                uqa_sql::ast::FunctionParamMode::In
                | uqa_sql::ast::FunctionParamMode::InOut
                | uqa_sql::ast::FunctionParamMode::Variadic => true,
                uqa_sql::ast::FunctionParamMode::Out => def.is_procedure,
                uqa_sql::ast::FunctionParamMode::Table => false,
            };
            if takes_argument {
                let value = bound.next().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "PL/pgSQL routine `{}` ran out of validated arguments while binding parameter {}",
                        def.name,
                        idx + 1
                    ))
                })?;
                if !matches!(param.mode, uqa_sql::ast::FunctionParamMode::Out) {
                    interpreter.values[idx] = value;
                }
            }
        }
        if bound.next().is_some() {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL routine `{}` left validated arguments unbound",
                def.name
            )));
        }
        // Each block initializes its own declared variables when reached.
        if let Some(found) = interpreter.found {
            interpreter.values[found] = Value::Bool(false);
        }
        Ok(interpreter)
    }

    pub fn into_outcome(self) -> RoutineOutcome {
        let out_values = self
            .out_datums
            .iter()
            .map(|idx| self.values[*idx].clone())
            .collect();
        RoutineOutcome {
            value: self.ret,
            out_values,
            set_rows: self.set_rows,
            anonymous_record_column_types: self.ret_record_types,
            sql_result_kind: None,
        }
    }

    pub fn initialize_trigger_context(
        &mut self,
        parsed: &PLpgSQLFunction,
        context: &super::TriggerRoutineContext,
    ) -> Result<(), SQLError> {
        if let Some(index) = parsed.new_datum {
            self.values[index] = context.new.clone();
            self.record_types
                .insert(index, context.column_types.clone());
        }
        if let Some(index) = parsed.old_datum {
            self.values[index] = context.old.clone();
            self.record_types
                .insert(index, context.column_types.clone());
        }
        let argument_values = context
            .arguments
            .iter()
            .cloned()
            .map(Value::Str)
            .collect::<Vec<_>>();
        let arguments = if argument_values.is_empty() {
            uqa_core::ArrayValue::try_new(argument_values)
        } else {
            uqa_core::ArrayValue::with_lower_bounds(argument_values, vec![0])
        }
        .ok_or_else(|| SQLError::Internal("trigger arguments are not a valid text array".into()))?;
        for (index, datum) in self.datums.iter().enumerate() {
            let Some(name) = datum.name() else {
                continue;
            };
            let value = match name.to_ascii_lowercase().as_str() {
                "new" => Some(context.new.clone()),
                "old" => Some(context.old.clone()),
                "tg_name" => Some(Value::Str(context.name.clone())),
                "tg_when" => Some(Value::Str(context.when.clone())),
                "tg_level" => Some(Value::Str(context.level.clone())),
                "tg_op" => Some(Value::Str(context.operation.clone())),
                "tg_relid" => Some(Value::Int(context.relation_oid)),
                "tg_relname" | "tg_table_name" => Some(Value::Str(context.table_name.clone())),
                "tg_table_schema" => Some(Value::Str(context.table_schema.clone())),
                "tg_nargs" => Some(Value::Int(i64::try_from(context.arguments.len()).map_err(
                    |_| SQLError::Internal("trigger argument count exceeds i64".into()),
                )?)),
                "tg_argv" => Some(Value::Array(arguments.clone())),
                _ => None,
            };
            if let Some(value) = value {
                self.values[index] = value;
            }
        }
        Ok(())
    }

    pub fn run(&mut self, action: &PLpgSQLBlock) -> Result<(), SQLError> {
        match self.exec_block(action)? {
            Flow::Return => Ok(()),
            Flow::Normal => {
                let returns_void = matches!(
                    &self.def.returns,
                    FunctionReturns::Scalar { type_name } if type_name == "void"
                );
                if self.def.is_procedure
                    || self.is_set
                    || returns_void
                    || !self.out_datums.is_empty()
                    || matches!(self.def.returns, FunctionReturns::None)
                {
                    Ok(())
                } else {
                    Err(SQLError::Routine {
                        sqlstate: "2F005".into(),
                        message: "control reached end of function without RETURN".into(),
                    })
                }
            }
            Flow::Exit(_) => Err(SQLError::Internal(
                "EXIT escaped every enclosing loop and block".into(),
            )),
            Flow::Continue(_) => Err(SQLError::Internal(
                "CONTINUE escaped every enclosing loop".into(),
            )),
        }
    }

    // -- expression / query plumbing -----------------------------------

    pub(super) fn resolver(&self) -> DatumResolver<'_> {
        DatumResolver {
            services: self.services,
            datums: self.datums,
            values: &self.values,
            record_types: &self.record_types,
            bindings: &self.bindings,
            error: self.err_stack.last().map(|error| &error.diagnostics),
            param_count: self.def.params.len(),
        }
    }

    pub(super) fn eval_expr(&self, expr: &PLpgSQLExpression) -> Result<Value, SQLError> {
        self.eval_expr_with_type(expr).map(|(value, _)| value)
    }

    pub(super) fn eval_expr_with_type(
        &self,
        expr: &PLpgSQLExpression,
    ) -> Result<(Value, Option<ColumnType>), SQLError> {
        let prepared = self.prepare_expression(expr)?;
        let result = self.execute_fragment(&prepared)?;
        if result.rows.len() > 1 {
            return Err(SQLError::Routine {
                sqlstate: "21000".into(),
                message: "query returned more than one row".into(),
            });
        }
        if result.columns.len() != 1 {
            return Err(SQLError::Internal(
                "PL/pgSQL expression returned multiple columns".into(),
            ));
        }
        Ok((
            result.value_at(0, 0).cloned().unwrap_or(Value::Null),
            result.column_types.first().cloned().flatten(),
        ))
    }

    /// How each variable site of `statement` resolves in the scope the statement runs in.
    pub(super) fn resolve_variable_sites(
        &self,
        statement: Statement,
        params: &[SQLParam],
        names: Vec<ScalarExpr>,
    ) -> Result<Vec<VariableSiteResolution>, SQLError> {
        let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
            self.services.runtime.has_aggregate_function(name)
        });
        let mut names = Some(names);
        let mut resolutions = Vec::new();
        self.services
            .statements
            .with_statement_scope(&mut |routines, ctes| {
                resolutions = resolve_variable_sites(
                    routines,
                    &mut plan,
                    params,
                    ctes,
                    names.take().unwrap_or_default(),
                )?;
                Ok(())
            })?;
        Ok(resolutions)
    }

    pub(super) fn eval_boolean(&self, expr: &PLpgSQLExpression) -> Result<Option<bool>, SQLError> {
        let (value, declared_type) = self.eval_expr_with_type(expr)?;
        let source_type = declared_type.as_ref().map(coercion_type_name);
        match cast_value_from(&value, "boolean", source_type.as_deref())? {
            Value::Null => Ok(None),
            Value::Bool(value) => Ok(Some(value)),
            other => Err(SQLError::Internal(format!(
                "PL/pgSQL boolean coercion returned {other:?}"
            ))),
        }
    }

    pub(super) fn exec_query(&self, statement: &PLpgSQLStatement) -> Result<SQLResult, SQLError> {
        let prepared = self.prepare_statement(statement)?;
        self.execute_fragment(&prepared)
    }

    pub(super) fn set_found(&mut self, value: bool) {
        if let Some(idx) = self.found {
            self.values[idx] = Value::Bool(value);
        }
    }

    pub(super) fn push_binding(&mut self, name: &str, idx: usize) {
        self.bindings.entry(name.to_string()).or_default().push(idx);
    }

    pub(super) fn pop_binding(&mut self, name: &str) {
        if let Some(stack) = self.bindings.get_mut(name) {
            stack.pop();
            if stack.is_empty() {
                self.bindings.remove(name);
            }
        }
    }
}
