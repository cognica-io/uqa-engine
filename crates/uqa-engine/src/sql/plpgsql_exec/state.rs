//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpreter activation state, expression binding, and routine lifecycle.

use super::{
    best_effort_cast, bind_expr, bind_statement, eval_lowered_expression,
    execute_compiled_statement, BTreeSet, CreateFunction, DatumResolver, Engine, Expr, Flow,
    FunctionReturns, HashMap, Interpreter, PLpgSQLBlock, PLpgSQLDatum, PLpgSQLFunction,
    RoutineOutcome, SQLError, SQLResult, Statement, Value,
};

impl<'a> Interpreter<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        def: &'a CreateFunction,
        parsed: &'a PLpgSQLFunction,
        bound: Vec<Value>,
    ) -> Result<Self, SQLError> {
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
        let loop_vars: BTreeSet<usize> = parsed.fori_variable_datums();
        let mut bindings: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, datum) in datums.iter().enumerate() {
            if loop_vars.contains(&idx) {
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
            engine,
            def,
            datums,
            values: vec![Value::Null; datums.len()],
            bindings,
            err_stack: Vec::new(),
            set_rows: Vec::new(),
            ret: Value::Null,
            out_datums,
            found: parsed.found_datum,
            last_row_count: 0,
            is_set: def.returns_set(),
        };
        // Bind call arguments onto the leading parameter datums.
        // Procedure OUT arguments start NULL (the placeholder value a
        // caller passes is discarded, matching PostgreSQL 14+).
        let mut bound = bound.into_iter();
        for (idx, param) in def.params.iter().enumerate() {
            let takes_argument = match param.mode {
                uqa_sql::ast::FunctionParamMode::In | uqa_sql::ast::FunctionParamMode::InOut => {
                    true
                }
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
        // Initialize FOUND and declared-variable defaults.
        if let Some(found) = interpreter.found {
            interpreter.values[found] = Value::Bool(false);
        }
        for (idx, datum) in datums.iter().enumerate().skip(def.params.len()) {
            let PLpgSQLDatum::Var(var) = datum else {
                continue;
            };
            if var.name.eq_ignore_ascii_case("found")
                || var.name.eq_ignore_ascii_case("sqlstate")
                || var.name.eq_ignore_ascii_case("sqlerrm")
            {
                continue;
            }
            if let Some(default) = &var.default {
                let value = interpreter.eval_expr(default)?;
                let value = best_effort_cast(&value, &var.type_name)?;
                interpreter.values[idx] = value;
            }
            if var.not_null && matches!(interpreter.values[idx], Value::Null) {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: format!(
                        "null value cannot be assigned to variable \"{}\" declared NOT NULL",
                        var.name
                    ),
                });
            }
        }
        Ok(interpreter)
    }

    pub(super) fn into_outcome(self) -> RoutineOutcome {
        let out_values = self
            .out_datums
            .iter()
            .map(|idx| self.values[*idx].clone())
            .collect();
        RoutineOutcome {
            value: self.ret,
            out_values,
            set_rows: self.set_rows,
        }
    }

    pub(super) fn run(&mut self, action: &PLpgSQLBlock) -> Result<(), SQLError> {
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
            datums: self.datums,
            values: &self.values,
            bindings: &self.bindings,
            error: self.err_stack.last(),
            param_count: self.def.params.len(),
        }
    }

    pub(super) fn eval_expr(&self, expr: &Expr) -> Result<Value, SQLError> {
        let bound = bind_expr(expr, &mut self.resolver())?;
        eval_lowered_expression(self.engine, &bound, None, &[])
    }

    pub(super) fn exec_query(&self, statement: &Statement) -> Result<SQLResult, SQLError> {
        let bound = bind_statement(statement, &mut self.resolver())?;
        execute_compiled_statement(self.engine, bound, &[])
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
