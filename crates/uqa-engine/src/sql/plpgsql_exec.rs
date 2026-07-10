//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution of user-defined routines: the `PL/pgSQL` interpreter,
//! the `LANGUAGE sql` body runner, and the `CREATE FUNCTION` / `DROP
//! FUNCTION` / `DO` / `CALL` statement handlers.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use uqa_sql::ast::{CreateFunction, DropFunctionStmt, Expr, FunctionReturns, Statement};
use uqa_sql::expr::{cast_value, evaluate_call_args, truthy, value_type_name, EvalContext};
use uqa_sql::plpgsql::{
    bind_expr, bind_statement, condition_sqlstate, IntoTarget, PLpgSQLBlock, PLpgSQLDatum,
    PLpgSQLFunction, PLpgSQLStmt, RaiseLevel, VariableResolver,
};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};

use crate::engine_user_functions::{CompiledFunctionBody, SQLUserFunction};
use crate::{Engine, SQLTableFunctionResult};

use super::run_optimized_stmt;
use std::sync::Arc;

use uqa_core::Value;

// ---------------------------------------------------------------------
// Statement handlers
// ---------------------------------------------------------------------

pub(super) fn run_create_function(
    engine: &Engine,
    def: CreateFunction,
) -> Result<SQLResult, SQLError> {
    engine.register_sql_function(def)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_drop_function(
    engine: &Engine,
    stmt: &DropFunctionStmt,
) -> Result<SQLResult, SQLError> {
    engine.drop_sql_functions(stmt)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_do_block(
    engine: &Engine,
    language: &str,
    body: &str,
) -> Result<SQLResult, SQLError> {
    if language != "plpgsql" {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{language}\" does not exist"),
        });
    }
    let parsed = uqa_sql::plpgsql::parse_do_block(body)?;
    let def = CreateFunction {
        name: "inline_code_block".into(),
        or_replace: false,
        is_procedure: false,
        params: Vec::new(),
        returns: FunctionReturns::Scalar {
            type_name: "void".into(),
        },
        language: "plpgsql".into(),
        body: uqa_sql::ast::FunctionBody::Source(body.to_string()),
        volatility: uqa_sql::ast::FunctionVolatility::Volatile,
        strict: false,
    };
    let _guard = DepthGuard::enter(engine)?;
    let mut interpreter = Interpreter::new(engine, &def, &parsed, Vec::new())?;
    interpreter.run(&parsed.action)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_call(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let call_args = evaluate_call_args(args, &ctx)?;
    let function = match resolve_routine(engine, name, &call_args, "procedure")? {
        Some(resolved) => resolved,
        None => {
            return Err(routine_resolution_error(
                "procedure",
                name,
                &call_args,
                "does not exist",
            ));
        }
    };
    let (function, bound) = function;
    if !function.def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is not a procedure", call_signature(name, &call_args)),
        });
    }
    let outcome = execute_routine(engine, &function, bound)?;
    let out_params = function.def.output_params();
    if out_params.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns = output_column_names(&function.def);
    let mut row = ResultRow::new();
    for (column, value) in columns.iter().zip(outcome.out_values.iter()) {
        row.insert(column.clone(), value.clone());
    }
    Ok(SQLResult {
        columns,
        rows: vec![row],
        affected_rows: 0,
    })
}

/// Scalar-context invocation used by the expression evaluator's
/// engine hook. `None` means no routine with this name exists.
pub(crate) fn call_user_scalar_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, "function") {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    let (function, bound) = resolved;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        }));
    }
    if function.def.returns_set() {
        return Some(Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "set-valued function called in context that cannot accept a set".into(),
        }));
    }
    if function.def.strict && bound.iter().any(|v| matches!(v, Value::Null)) {
        return Some(Ok(Value::Null));
    }
    let outcome = match execute_routine(engine, &function, bound) {
        Ok(outcome) => outcome,
        Err(e) => return Some(Err(e)),
    };
    let out_params = function.def.output_params();
    let value = match out_params.len() {
        0 => outcome.value,
        1 => outcome.out_values.into_iter().next().unwrap_or(Value::Null),
        _ => {
            let mut record = BTreeMap::new();
            for (column, value) in output_column_names(&function.def)
                .into_iter()
                .zip(outcome.out_values)
            {
                record.insert(column, value);
            }
            Value::Map(record)
        }
    };
    Some(Ok(value))
}

/// FROM-clause invocation: any user routine is callable as a table
/// source (`SELECT * FROM f(...)`); scalar functions produce a single
/// row. `None` means no routine with this name exists.
pub(crate) fn call_user_table_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, "function") {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    let (function, bound) = resolved;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        }));
    }
    let out_params = function.def.output_params();
    let columns = if out_params.is_empty() {
        vec![function.def.name.clone()]
    } else {
        output_column_names(&function.def)
    };
    if function.def.strict && bound.iter().any(|v| matches!(v, Value::Null)) {
        let rows = if function.def.returns_set() {
            Vec::new()
        } else {
            vec![vec![Value::Null; columns.len()]]
        };
        return Some(Ok(SQLTableFunctionResult::new(columns, rows)));
    }
    let outcome = match execute_routine(engine, &function, bound) {
        Ok(outcome) => outcome,
        Err(e) => return Some(Err(e)),
    };
    let rows = if function.def.returns_set() {
        outcome.set_rows
    } else if out_params.is_empty() {
        vec![vec![outcome.value]]
    } else {
        vec![outcome.out_values]
    };
    Some(Ok(SQLTableFunctionResult::new(columns, rows)))
}

// ---------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------

fn output_column_names(def: &CreateFunction) -> Vec<String> {
    def.output_params()
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            if p.name.is_empty() {
                format!("column{}", idx + 1)
            } else {
                p.name.clone()
            }
        })
        .collect()
}

fn call_signature(name: &str, args: &[(Option<String>, Value)]) -> String {
    let types = args
        .iter()
        .map(|(arg_name, value)| match arg_name {
            Some(arg_name) => format!("{arg_name} => {}", value_type_name(value)),
            None => value_type_name(value).to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({types})")
}

fn routine_resolution_error(
    kind: &str,
    name: &str,
    args: &[(Option<String>, Value)],
    suffix: &str,
) -> SQLError {
    SQLError::Routine {
        sqlstate: if suffix == "is not unique" {
            "42725".into()
        } else {
            "42883".into()
        },
        message: format!("{kind} {} {suffix}", call_signature(name, args)),
    }
}

/// One argument slot after structural matching.
enum ArgSlot {
    Filled(Value),
    NeedsDefault(usize),
}

/// Match a call's argument list against a routine signature without
/// evaluating defaults. `None` = not a candidate.
fn try_match_arguments(
    def: &CreateFunction,
    args: &[(Option<String>, Value)],
) -> Option<Vec<ArgSlot>> {
    let signature = def.signature_params();
    if args.len() > signature.len() {
        return None;
    }
    let mut slots: Vec<Option<ArgSlot>> = (0..signature.len()).map(|_| None).collect();
    let mut position = 0usize;
    let mut seen_named = false;
    for (arg_name, value) in args {
        match arg_name {
            None => {
                if seen_named || position >= signature.len() {
                    return None;
                }
                slots[position] = Some(ArgSlot::Filled(value.clone()));
                position += 1;
            }
            Some(arg_name) => {
                seen_named = true;
                let idx = signature
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(arg_name))?;
                if slots[idx].is_some() {
                    return None;
                }
                slots[idx] = Some(ArgSlot::Filled(value.clone()));
            }
        }
    }
    let mut out = Vec::with_capacity(slots.len());
    for (idx, slot) in slots.into_iter().enumerate() {
        if let Some(slot) = slot {
            out.push(slot);
        } else {
            signature[idx].default.as_ref()?;
            out.push(ArgSlot::NeedsDefault(idx));
        }
    }
    Some(out)
}

/// A resolved overload plus its bound argument values.
type ResolvedRoutine = (Arc<SQLUserFunction>, Vec<Value>);

/// Resolve `name(args)` to a single overload and its bound argument
/// values (declared-type casts applied, defaults evaluated).
/// `Ok(None)` = no routine with this name at all.
fn resolve_routine(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
    kind: &str,
) -> Result<Option<ResolvedRoutine>, SQLError> {
    let Some(overloads) = engine.lookup_sql_functions(name) else {
        return Ok(None);
    };
    let mut candidates: Vec<(Arc<SQLUserFunction>, Vec<ArgSlot>)> = Vec::new();
    for function in overloads {
        if let Some(slots) = try_match_arguments(&function.def, args) {
            candidates.push((function, slots));
        }
    }
    match candidates.len() {
        0 => Err(routine_resolution_error(kind, name, args, "does not exist")),
        1 => {
            let (function, slots) = candidates.remove(0);
            let bound = materialize_arguments(engine, &function.def, slots)?;
            Ok(Some((function, bound)))
        }
        _ => Err(routine_resolution_error(kind, name, args, "is not unique")),
    }
}

/// Evaluate defaults and apply declared-type casts for the winning
/// overload.
fn materialize_arguments(
    engine: &Engine,
    def: &CreateFunction,
    slots: Vec<ArgSlot>,
) -> Result<Vec<Value>, SQLError> {
    let signature = def.signature_params();
    let ctx = EvalContext::new(None, &[]).with_engine(engine);
    let mut bound = Vec::with_capacity(slots.len());
    for (idx, slot) in slots.into_iter().enumerate() {
        let value = match slot {
            ArgSlot::Filled(value) => value,
            ArgSlot::NeedsDefault(param_idx) => {
                let default = signature[param_idx].default.as_ref().ok_or_else(|| {
                    SQLError::Internal("argument default vanished during resolution".into())
                })?;
                uqa_sql::expr::eval(default, &ctx)?
            }
        };
        bound.push(best_effort_cast(&value, &signature[idx].type_name)?);
    }
    Ok(bound)
}

/// Cast through the SQL value layer; unknown target types keep the
/// value unchanged (`%TYPE`, `record`, domain names, ...).
fn best_effort_cast(value: &Value, type_name: &str) -> Result<Value, SQLError> {
    match cast_value(value, type_name) {
        Ok(value) => Ok(value),
        Err(SQLError::Unsupported(_)) => Ok(value.clone()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------
// Routine execution
// ---------------------------------------------------------------------

thread_local! {
    static CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
    static STACK_BASE: Cell<usize> = const { Cell::new(0) };
}

/// Native stack budget for nested routine calls, measured from the
/// outermost routine entry. The `PostgreSQL` `max_stack_depth`
/// setting plays the same role (default 2MB there); this budget is
/// sized so the guard
/// always fires before a 2MB thread stack (the Rust test-runner
/// default) is exhausted, even in debug builds.
const STACK_BYTE_BUDGET: usize = 1_000_000;

/// Approximate current stack position.
#[inline(never)]
fn stack_marker() -> usize {
    let marker = 0u8;
    std::ptr::from_ref(&marker) as usize
}

fn stack_depth_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "54001".into(),
        message: "stack depth limit exceeded".into(),
    }
}

/// RAII guard for the user-routine nesting caps: a configurable
/// frame-count limit plus a native stack-byte budget.
struct DepthGuard;

impl DepthGuard {
    fn enter(engine: &Engine) -> Result<Self, SQLError> {
        let depth = CALL_DEPTH.get();
        if depth == 0 {
            STACK_BASE.set(stack_marker());
        } else if STACK_BASE.get().abs_diff(stack_marker()) > STACK_BYTE_BUDGET {
            return Err(stack_depth_error());
        }
        if depth >= engine.sql_function_depth_limit() {
            return Err(stack_depth_error());
        }
        CALL_DEPTH.set(depth + 1);
        Ok(Self)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        CALL_DEPTH.set(CALL_DEPTH.get().saturating_sub(1));
    }
}

/// Result of one routine execution before context shaping.
struct RoutineOutcome {
    /// Explicit `RETURN expr` value (scalar functions).
    value: Value,
    /// Final values of OUT / INOUT / TABLE parameters, in order.
    out_values: Vec<Value>,
    /// Accumulated result set (`RETURN NEXT` / `RETURN QUERY`).
    set_rows: Vec<Vec<Value>>,
}

fn execute_routine(
    engine: &Engine,
    function: &SQLUserFunction,
    bound: Vec<Value>,
) -> Result<RoutineOutcome, SQLError> {
    let _guard = DepthGuard::enter(engine)?;
    match &function.compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            let mut interpreter = Interpreter::new(engine, &function.def, parsed, bound)?;
            interpreter.run(&parsed.action)?;
            Ok(interpreter.into_outcome())
        }
        CompiledFunctionBody::SQL(statements) => {
            execute_sql_language(engine, &function.def, statements, &bound)
        }
    }
}

/// `LANGUAGE sql` body: run every statement; the last statement's
/// result shapes the routine output.
fn execute_sql_language(
    engine: &Engine,
    def: &CreateFunction,
    statements: &[Statement],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    let mut resolver = SQLFunctionResolver { def, bound };
    let mut last = SQLResult::empty();
    for statement in statements {
        let statement = bind_statement(statement, &mut resolver)?;
        last = run_optimized_stmt(engine, statement, &[])?;
    }
    let out_params = def.output_params();
    let returns_void = matches!(
        &def.returns,
        FunctionReturns::Scalar { type_name } if type_name == "void"
    );
    let expected = if out_params.is_empty() {
        1
    } else {
        out_params.len()
    };
    // PostgreSQL enforces the final statement's column shape at
    // CREATE time; the engine has no schema binding there, so the
    // same 42P13 error surfaces on the first call instead.
    let shape_checked = !returns_void && (!def.is_procedure || !out_params.is_empty());
    if shape_checked && last.columns.len() != expected {
        return Err(sql_body_shape_error(def));
    }
    let row_values = |row: &ResultRow| -> Vec<Value> {
        last.columns
            .iter()
            .map(|column| row_value(row, column))
            .collect()
    };
    if def.returns_set() {
        let mut set_rows = Vec::with_capacity(last.rows.len());
        for row in &last.rows {
            let mut values = row_values(row);
            if values.len() != expected {
                return Err(sql_body_shape_error(def));
            }
            if out_params.is_empty() {
                if let FunctionReturns::SetOf { type_name } = &def.returns {
                    values[0] = best_effort_cast(&values[0], type_name)?;
                }
            }
            set_rows.push(values);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; out_params.len()],
            set_rows,
        });
    }
    let first = last.rows.first().map(row_values);
    if !out_params.is_empty() {
        let mut out_values = vec![Value::Null; out_params.len()];
        if let Some(values) = first {
            for (idx, value) in values.into_iter().take(out_values.len()).enumerate() {
                out_values[idx] = value;
            }
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values,
            set_rows: Vec::new(),
        });
    }
    let value = match first {
        Some(values) if returns_void => {
            let _ = values;
            Value::Null
        }
        Some(mut values) => {
            if values.is_empty() {
                Value::Null
            } else {
                let value = values.remove(0);
                match &def.returns {
                    FunctionReturns::Scalar { type_name } => best_effort_cast(&value, type_name)?,
                    _ => value,
                }
            }
        }
        None => Value::Null,
    };
    Ok(RoutineOutcome {
        value,
        out_values: Vec::new(),
        set_rows: Vec::new(),
    })
}

fn sql_body_shape_error(def: &CreateFunction) -> SQLError {
    let declared = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            type_name.clone()
        }
        FunctionReturns::Table => "record".into(),
        FunctionReturns::None => "record".into(),
    };
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: format!("return type mismatch in function declared to return {declared}"),
    }
}

/// Resolver for `LANGUAGE sql` bodies: parameter names and `$n`
/// references bind to the call's argument values.
struct SQLFunctionResolver<'a> {
    def: &'a CreateFunction,
    bound: &'a [Value],
}

impl VariableResolver for SQLFunctionResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<Value>, SQLError> {
        let position = self
            .def
            .signature_params()
            .iter()
            .position(|p| !p.name.is_empty() && p.name.eq_ignore_ascii_case(name));
        Ok(position.and_then(|idx| self.bound.get(idx).cloned()))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Value>, SQLError> {
        // `fname.param` qualification is accepted by PostgreSQL; the
        // qualifier must be the function name.
        if qualifier.eq_ignore_ascii_case(&self.def.name) {
            return self.resolve_name(column);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<Value>, SQLError> {
        if index >= 1 && index <= self.bound.len() {
            Ok(Some(self.bound[index - 1].clone()))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------
// PL/pgSQL interpreter
// ---------------------------------------------------------------------

/// Control flow signal propagating out of statement execution.
enum Flow {
    Normal,
    Exit(Option<String>),
    Continue(Option<String>),
    Return,
}

struct Interpreter<'a> {
    engine: &'a Engine,
    def: &'a CreateFunction,
    datums: &'a [PLpgSQLDatum],
    values: Vec<Value>,
    /// `name -> datum index` stacks; the innermost binding wins.
    bindings: HashMap<String, Vec<usize>>,
    /// `(SQLSTATE, SQLERRM)` stack while exception handlers run.
    err_stack: Vec<(String, String)>,
    set_rows: Vec<Vec<Value>>,
    ret: Value,
    out_datums: Vec<usize>,
    found: Option<usize>,
    last_row_count: i64,
    is_set: bool,
}

impl<'a> Interpreter<'a> {
    fn new(
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
        let mut next_arg = 0usize;
        for (idx, param) in def.params.iter().enumerate() {
            let takes_argument = match param.mode {
                uqa_sql::ast::FunctionParamMode::In | uqa_sql::ast::FunctionParamMode::InOut => {
                    true
                }
                uqa_sql::ast::FunctionParamMode::Out => def.is_procedure,
                uqa_sql::ast::FunctionParamMode::Table => false,
            };
            if takes_argument {
                let value = bound.get(next_arg).cloned().unwrap_or(Value::Null);
                next_arg += 1;
                if !matches!(param.mode, uqa_sql::ast::FunctionParamMode::Out) {
                    interpreter.values[idx] = value;
                }
            }
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

    fn into_outcome(self) -> RoutineOutcome {
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

    fn run(&mut self, action: &PLpgSQLBlock) -> Result<(), SQLError> {
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

    fn resolver(&self) -> DatumResolver<'_> {
        DatumResolver {
            datums: self.datums,
            values: &self.values,
            bindings: &self.bindings,
            error: self.err_stack.last(),
            param_count: self.def.params.len(),
        }
    }

    fn eval_expr(&self, expr: &Expr) -> Result<Value, SQLError> {
        let bound = bind_expr(expr, &mut self.resolver())?;
        let ctx = EvalContext::new(None, &[]).with_engine(self.engine);
        uqa_sql::expr::eval(&bound, &ctx)
    }

    fn exec_query(&self, statement: &Statement) -> Result<SQLResult, SQLError> {
        let bound = bind_statement(statement, &mut self.resolver())?;
        run_optimized_stmt(self.engine, bound, &[])
    }

    fn set_found(&mut self, value: bool) {
        if let Some(idx) = self.found {
            self.values[idx] = Value::Bool(value);
        }
    }

    fn push_binding(&mut self, name: &str, idx: usize) {
        self.bindings.entry(name.to_string()).or_default().push(idx);
    }

    fn pop_binding(&mut self, name: &str) {
        if let Some(stack) = self.bindings.get_mut(name) {
            stack.pop();
            if stack.is_empty() {
                self.bindings.remove(name);
            }
        }
    }

    // -- datum assignment ----------------------------------------------

    fn datum_name(&self, idx: usize) -> String {
        self.datums[idx].name().unwrap_or("?").to_string()
    }

    /// Store into a datum applying CONSTANT / type / NOT NULL rules.
    fn assign_datum(&mut self, idx: usize, value: Value) -> Result<(), SQLError> {
        match &self.datums[idx] {
            PLpgSQLDatum::Var(var) => {
                if var.constant {
                    return Err(SQLError::Routine {
                        sqlstate: "22005".into(),
                        message: format!("variable \"{}\" is declared CONSTANT", var.name),
                    });
                }
                let value = best_effort_cast(&value, &var.type_name)?;
                if var.not_null && matches!(value, Value::Null) {
                    return Err(SQLError::Routine {
                        sqlstate: "22004".into(),
                        message: format!(
                            "null value cannot be assigned to variable \"{}\" declared NOT NULL",
                            var.name
                        ),
                    });
                }
                self.values[idx] = value;
                Ok(())
            }
            PLpgSQLDatum::Rec { .. } => match value {
                Value::Map(_) | Value::Null => {
                    self.values[idx] = value;
                    Ok(())
                }
                _ => Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot assign non-composite value to a record variable".into(),
                }),
            },
            PLpgSQLDatum::RecField { field, parent } => {
                let parent_name = self.datum_name(*parent);
                match &mut self.values[*parent] {
                    Value::Map(map) => {
                        let key = map
                            .keys()
                            .find(|k| k.eq_ignore_ascii_case(field))
                            .cloned()
                            .ok_or_else(|| SQLError::Routine {
                                sqlstate: "42703".into(),
                                message: format!(
                                    "record \"{parent_name}\" has no field \"{field}\""
                                ),
                            })?;
                        map.insert(key, value);
                        Ok(())
                    }
                    _ => Err(SQLError::Routine {
                        sqlstate: "55000".into(),
                        message: format!("record \"{parent_name}\" is not assigned yet"),
                    }),
                }
            }
            PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(
                "direct assignment to a row datum".into(),
            )),
        }
    }

    /// Assign a query result row (or NULLs) to an INTO target.
    fn assign_into(
        &mut self,
        target: &IntoTarget,
        columns: &[String],
        row: Option<&ResultRow>,
    ) -> Result<(), SQLError> {
        match target {
            IntoTarget::Rec(dno) => {
                let value = match row {
                    Some(row) => {
                        let mut record = BTreeMap::new();
                        for column in columns {
                            record.insert(column.clone(), row_value(row, column));
                        }
                        Value::Map(record)
                    }
                    None => Value::Null,
                };
                self.values[*dno] = value;
                Ok(())
            }
            IntoTarget::Row(fields) => {
                for (idx, field) in fields.iter().enumerate() {
                    let value = match (row, columns.get(idx)) {
                        (Some(row), Some(column)) => row_value(row, column),
                        _ => Value::Null,
                    };
                    self.assign_datum(field.varno, value)?;
                }
                Ok(())
            }
        }
    }

    // -- blocks and statements -------------------------------------------

    /// Run one block, routing failures through its EXCEPTION arms.
    ///
    /// Divergence from `PostgreSQL`: a caught error does not roll
    /// back data changes the block made before failing (stock
    /// `PL/pgSQL` wraps exception-guarded blocks in a
    /// subtransaction). Handlers here only recover control flow.
    fn exec_block(&mut self, block: &PLpgSQLBlock) -> Result<Flow, SQLError> {
        let result = self.exec_stmts(&block.body);
        let result = match result {
            Err(error) if !block.exceptions.is_empty() && catchable(&error) => {
                let state = error.sqlstate().unwrap_or("XX000").to_string();
                let message = routine_message(&error);
                let arm = block
                    .exceptions
                    .iter()
                    .find(|arm| arm_matches(&arm.conditions, &state));
                match arm {
                    Some(arm) => {
                        self.err_stack.push((state, message));
                        let handled = self.exec_stmts(&arm.body);
                        self.err_stack.pop();
                        handled
                    }
                    None => Err(error),
                }
            }
            other => other,
        };
        match result {
            Ok(Flow::Exit(Some(label))) if block.label.as_deref() == Some(label.as_str()) => {
                Ok(Flow::Normal)
            }
            other => other,
        }
    }

    fn exec_stmts(&mut self, stmts: &[PLpgSQLStmt]) -> Result<Flow, SQLError> {
        for stmt in stmts {
            match self.exec_stmt(stmt)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, stmt: &PLpgSQLStmt) -> Result<Flow, SQLError> {
        self.engine.cancellation_token().check()?;
        match stmt {
            PLpgSQLStmt::Block(block) => self.exec_block(block),
            PLpgSQLStmt::Assign { target, expr } => {
                let value = self.eval_expr(expr)?;
                self.assign_datum(*target, value)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::If {
                cond,
                then_body,
                elsifs,
                else_body,
            } => {
                if truthy(&self.eval_expr(cond)?) {
                    return self.exec_stmts(then_body);
                }
                for (elsif_cond, body) in elsifs {
                    if truthy(&self.eval_expr(elsif_cond)?) {
                        return self.exec_stmts(body);
                    }
                }
                match else_body {
                    Some(body) => self.exec_stmts(body),
                    None => Ok(Flow::Normal),
                }
            }
            PLpgSQLStmt::Case {
                t_expr,
                t_varno,
                arms,
                else_body,
            } => {
                if let (Some(t_expr), Some(varno)) = (t_expr, t_varno) {
                    let value = self.eval_expr(t_expr)?;
                    self.values[*varno] = value;
                }
                for (cond, body) in arms {
                    if truthy(&self.eval_expr(cond)?) {
                        return self.exec_stmts(body);
                    }
                }
                match else_body {
                    Some(body) => self.exec_stmts(body),
                    None => Err(SQLError::Routine {
                        sqlstate: "20000".into(),
                        message: "case not found".into(),
                    }),
                }
            }
            PLpgSQLStmt::Loop { label, body } => loop {
                match self.exec_loop_body(label.as_deref(), body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => return Ok(Flow::Normal),
                    LoopSignal::Propagate(flow) => return Ok(flow),
                }
            },
            PLpgSQLStmt::While { label, cond, body } => loop {
                if !truthy(&self.eval_expr(cond)?) {
                    return Ok(Flow::Normal);
                }
                match self.exec_loop_body(label.as_deref(), body)? {
                    LoopSignal::Continue => {}
                    LoopSignal::Break => return Ok(Flow::Normal),
                    LoopSignal::Propagate(flow) => return Ok(flow),
                }
            },
            PLpgSQLStmt::ForI {
                label,
                var,
                lower,
                upper,
                step,
                reverse,
                body,
            } => {
                let name = self.datum_name(*var);
                self.push_binding(&name, *var);
                let result = self.exec_fori(
                    label.as_deref(),
                    *var,
                    lower,
                    upper,
                    step.as_ref(),
                    *reverse,
                    body,
                );
                self.pop_binding(&name);
                result
            }
            PLpgSQLStmt::ForQuery {
                label,
                target,
                query,
                body,
            } => {
                let result = self.exec_query(query)?;
                let mut iterated = false;
                let mut outcome = Flow::Normal;
                for row in &result.rows {
                    iterated = true;
                    self.assign_into(target, &result.columns, Some(row))?;
                    match self.exec_loop_body(label.as_deref(), body)? {
                        LoopSignal::Continue => {}
                        LoopSignal::Break => break,
                        LoopSignal::Propagate(flow) => {
                            outcome = flow;
                            break;
                        }
                    }
                }
                self.set_found(iterated);
                Ok(outcome)
            }
            PLpgSQLStmt::Exit {
                is_exit,
                label,
                cond,
            } => {
                if let Some(cond) = cond {
                    if !truthy(&self.eval_expr(cond)?) {
                        return Ok(Flow::Normal);
                    }
                }
                if *is_exit {
                    Ok(Flow::Exit(label.clone()))
                } else {
                    Ok(Flow::Continue(label.clone()))
                }
            }
            PLpgSQLStmt::Return { expr } => self.exec_return(expr.as_ref()),
            PLpgSQLStmt::ReturnNext { expr } => self.exec_return_next(expr.as_ref()),
            PLpgSQLStmt::ReturnQuery { query } => {
                if !self.is_set {
                    return Err(return_query_context_error());
                }
                let result = self.exec_query(query)?;
                self.append_query_rows(&result)?;
                // PostgreSQL sets ROW_COUNT (but not FOUND) here.
                self.last_row_count = result_row_count(&result);
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::ReturnQueryExecute { query, params } => {
                if !self.is_set {
                    return Err(return_query_context_error());
                }
                let result = self.exec_dynamic(query, params)?;
                self.append_query_rows(&result)?;
                self.last_row_count = result_row_count(&result);
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::Raise {
                level,
                condition,
                message,
                params,
            } => self.exec_raise(*level, condition.as_deref(), message.as_deref(), params),
            PLpgSQLStmt::ExecSQL { stmt, into, strict } => {
                let result = self.exec_query(stmt)?;
                self.consume_statement_result(stmt, &result, into.as_ref(), *strict)?;
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::DynExecute {
                query,
                params,
                into,
                strict,
            } => {
                let result = self.exec_dynamic(query, params)?;
                let row_count = result_row_count(&result);
                self.last_row_count = row_count;
                if let Some(target) = into {
                    if *strict {
                        strict_into_check(row_count)?;
                    }
                    self.assign_into(target, &result.columns, result.rows.first())?;
                }
                // PostgreSQL: EXECUTE never changes FOUND.
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::Perform { query } => {
                let result = self.exec_query(query)?;
                let row_count = result_row_count(&result);
                self.last_row_count = row_count;
                self.set_found(row_count > 0);
                Ok(Flow::Normal)
            }
            PLpgSQLStmt::GetDiagnostics { items } => {
                for (kind, target) in items {
                    match kind.as_str() {
                        "ROW_COUNT" => {
                            let count = Value::Int(self.last_row_count);
                            self.assign_datum(*target, count)?;
                        }
                        other => {
                            return Err(SQLError::Unsupported(format!("GET DIAGNOSTICS {other}")));
                        }
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }

    /// Run one loop iteration body and classify the resulting flow
    /// with respect to this loop's label.
    fn exec_loop_body(
        &mut self,
        label: Option<&str>,
        body: &[PLpgSQLStmt],
    ) -> Result<LoopSignal, SQLError> {
        match self.exec_stmts(body)? {
            Flow::Normal => Ok(LoopSignal::Continue),
            Flow::Continue(flow_label) => {
                if flow_label.is_none() || flow_label.as_deref() == label {
                    Ok(LoopSignal::Continue)
                } else {
                    Ok(LoopSignal::Propagate(Flow::Continue(flow_label)))
                }
            }
            Flow::Exit(flow_label) => {
                if flow_label.is_none() || flow_label.as_deref() == label {
                    Ok(LoopSignal::Break)
                } else {
                    Ok(LoopSignal::Propagate(Flow::Exit(flow_label)))
                }
            }
            Flow::Return => Ok(LoopSignal::Propagate(Flow::Return)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_fori(
        &mut self,
        label: Option<&str>,
        var: usize,
        lower: &Expr,
        upper: &Expr,
        step: Option<&Expr>,
        reverse: bool,
        body: &[PLpgSQLStmt],
    ) -> Result<Flow, SQLError> {
        let lower = self.eval_loop_bound(lower, "lower")?;
        let upper = self.eval_loop_bound(upper, "upper")?;
        let step_value = match step {
            Some(expr) => {
                let value = self.eval_expr(expr)?;
                if matches!(value, Value::Null) {
                    return Err(SQLError::Routine {
                        sqlstate: "22004".into(),
                        message: "BY value of FOR loop cannot be null".into(),
                    });
                }
                to_i64_value(&value)?
            }
            None => 1,
        };
        if step_value <= 0 {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: "BY value of FOR loop must be greater than zero".into(),
            });
        }
        let mut current = lower;
        let mut iterated = false;
        let mut outcome = Flow::Normal;
        loop {
            let done = if reverse {
                current < upper
            } else {
                current > upper
            };
            if done {
                break;
            }
            iterated = true;
            self.values[var] = Value::Int(current);
            match self.exec_loop_body(label, body)? {
                LoopSignal::Continue => {}
                LoopSignal::Break => break,
                LoopSignal::Propagate(flow) => {
                    outcome = flow;
                    break;
                }
            }
            if reverse {
                current -= step_value;
            } else {
                current += step_value;
            }
        }
        self.set_found(iterated);
        Ok(outcome)
    }

    fn eval_loop_bound(&self, expr: &Expr, which: &str) -> Result<i64, SQLError> {
        let value = self.eval_expr(expr)?;
        if matches!(value, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: format!("{which} bound of FOR loop cannot be null"),
            });
        }
        to_i64_value(&value)
    }

    fn exec_return(&mut self, expr: Option<&Expr>) -> Result<Flow, SQLError> {
        if let Some(expr) = expr {
            let context = if self.def.is_procedure {
                Some("RETURN cannot have a parameter in a procedure")
            } else if self.is_set {
                Some("RETURN cannot have a parameter in function returning set")
            } else if !self.out_datums.is_empty() {
                Some("RETURN cannot have a parameter in function with OUT parameters")
            } else if matches!(
                &self.def.returns,
                FunctionReturns::Scalar { type_name } if type_name == "void"
            ) {
                Some("RETURN cannot have a parameter in function returning void")
            } else {
                None
            };
            if let Some(message) = context {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: message.into(),
                });
            }
            let value = self.eval_expr(expr)?;
            self.ret = match &self.def.returns {
                FunctionReturns::Scalar { type_name } => best_effort_cast(&value, type_name)?,
                _ => value,
            };
            return Ok(Flow::Return);
        }
        // Bare RETURN (including the implicit one the parser appends
        // at the end of every body). A plain scalar function reaching
        // it has produced no value - PostgreSQL's runtime error.
        let returns_void = matches!(
            &self.def.returns,
            FunctionReturns::Scalar { type_name } if type_name == "void"
        );
        let implicit_ok = self.def.is_procedure
            || self.is_set
            || returns_void
            || !self.out_datums.is_empty()
            || matches!(self.def.returns, FunctionReturns::None);
        if !implicit_ok {
            return Err(SQLError::Routine {
                sqlstate: "2F005".into(),
                message: "control reached end of function without RETURN".into(),
            });
        }
        Ok(Flow::Return)
    }

    fn exec_return_next(&mut self, expr: Option<&Expr>) -> Result<Flow, SQLError> {
        if !self.is_set {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cannot use RETURN NEXT in a non-SETOF function".into(),
            });
        }
        if !self.out_datums.is_empty() {
            if expr.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "RETURN NEXT cannot have a parameter in function with OUT parameters"
                        .into(),
                });
            }
            let row = self
                .out_datums
                .iter()
                .map(|idx| self.values[*idx].clone())
                .collect();
            self.set_rows.push(row);
            return Ok(Flow::Normal);
        }
        let Some(expr) = expr else {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "RETURN NEXT must have a parameter".into(),
            });
        };
        let value = self.eval_expr(expr)?;
        let value = match &self.def.returns {
            FunctionReturns::SetOf { type_name } => best_effort_cast(&value, type_name)?,
            _ => value,
        };
        self.set_rows.push(vec![value]);
        Ok(Flow::Normal)
    }

    fn append_query_rows(&mut self, result: &SQLResult) -> Result<(), SQLError> {
        let expected = if self.out_datums.is_empty() {
            1
        } else {
            self.out_datums.len()
        };
        if result.columns.len() != expected {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "structure of query does not match function result type".into(),
            });
        }
        for row in &result.rows {
            let values = result
                .columns
                .iter()
                .map(|column| row_value(row, column))
                .collect();
            self.set_rows.push(values);
        }
        Ok(())
    }

    fn exec_raise(
        &mut self,
        level: RaiseLevel,
        condition: Option<&str>,
        message: Option<&str>,
        params: &[Expr],
    ) -> Result<Flow, SQLError> {
        // Bare RAISE re-throws the error being handled.
        if condition.is_none() && message.is_none() {
            return match self.err_stack.last() {
                Some((state, message)) => Err(SQLError::Routine {
                    sqlstate: state.clone(),
                    message: message.clone(),
                }),
                None => Err(SQLError::Routine {
                    sqlstate: "0Z002".into(),
                    message: "RAISE without parameters cannot be used outside an exception handler"
                        .into(),
                }),
            };
        }
        let text = match message {
            Some(format) => {
                let mut values = Vec::with_capacity(params.len());
                for param in params {
                    values.push(self.eval_expr(param)?);
                }
                format_raise_message(format, &values)?
            }
            None => condition.unwrap_or_default().to_string(),
        };
        if level == RaiseLevel::Error {
            let sqlstate = match condition {
                Some(name) => condition_sqlstate(name).map_or_else(
                    || {
                        if looks_like_sqlstate(name) {
                            name.to_ascii_uppercase()
                        } else {
                            "P0001".to_string()
                        }
                    },
                    ToString::to_string,
                ),
                None => "P0001".to_string(),
            };
            return Err(SQLError::Routine {
                sqlstate,
                message: text,
            });
        }
        self.engine.push_sql_notice(level.as_str(), &text);
        Ok(Flow::Normal)
    }

    fn exec_dynamic(&mut self, query: &Expr, params: &[Expr]) -> Result<SQLResult, SQLError> {
        let text = match self.eval_expr(query)? {
            Value::Str(text) => text,
            Value::Null => {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "query string argument of EXECUTE is null".into(),
                });
            }
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "EXECUTE expects a query string, got {other:?}"
                )));
            }
        };
        let mut bound_params = Vec::with_capacity(params.len());
        for param in params {
            bound_params.push(SQLParam::Scalar(self.eval_expr(param)?));
        }
        super::execute(self.engine, &text, &bound_params)
    }

    /// Post-process an embedded SQL statement's result: `ROW_COUNT`,
    /// `FOUND`, and `INTO` assignment.
    fn consume_statement_result(
        &mut self,
        statement: &Statement,
        result: &SQLResult,
        into: Option<&IntoTarget>,
        strict: bool,
    ) -> Result<(), SQLError> {
        let row_count = result_row_count(result);
        self.last_row_count = row_count;
        if let Some(target) = into {
            if strict {
                strict_into_check(row_count)?;
            }
            self.assign_into(target, &result.columns, result.rows.first())?;
        }
        // CALL statements leave FOUND untouched.
        if !matches!(statement, Statement::Call { .. }) {
            self.set_found(row_count > 0);
        }
        Ok(())
    }
}

/// Flow classification of one loop iteration.
enum LoopSignal {
    Continue,
    Break,
    Propagate(Flow),
}

fn return_query_context_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "cannot use RETURN QUERY in a non-SETOF function".into(),
    }
}

/// Column lookup with a qualified-key fallback: result rows may key
/// values as `table.column` while the column list carries the bare
/// label.
fn row_value(row: &ResultRow, column: &str) -> Value {
    if let Some(value) = row.get(column) {
        return value.clone();
    }
    row.iter()
        .find(|(key, _)| {
            key.rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix == column)
        })
        .map_or(Value::Null, |(_, value)| value.clone())
}

fn result_row_count(result: &SQLResult) -> i64 {
    if result.columns.is_empty() {
        i64::try_from(result.affected_rows).unwrap_or(i64::MAX)
    } else {
        i64::try_from(result.rows.len()).unwrap_or(i64::MAX)
    }
}

fn strict_into_check(row_count: i64) -> Result<(), SQLError> {
    if row_count == 0 {
        return Err(SQLError::Routine {
            sqlstate: "P0002".into(),
            message: "query returned no rows".into(),
        });
    }
    if row_count > 1 {
        return Err(SQLError::Routine {
            sqlstate: "P0003".into(),
            message: "query returned more than one row".into(),
        });
    }
    Ok(())
}

fn to_i64_value(value: &Value) -> Result<i64, SQLError> {
    match cast_value(value, "bigint")? {
        Value::Int(v) => Ok(v),
        other => Err(SQLError::TypeMismatch(format!(
            "expected an integer, got {other:?}"
        ))),
    }
}

fn catchable(error: &SQLError) -> bool {
    !matches!(error, SQLError::Cancelled(_))
}

/// Message text exposed through SQLERRM: user-routine errors keep
/// their raw message, engine errors keep their display form.
fn routine_message(error: &SQLError) -> String {
    match error {
        SQLError::Routine { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

fn looks_like_sqlstate(text: &str) -> bool {
    text.len() == 5 && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Match an exception arm's condition list against a `SQLSTATE`.
fn arm_matches(conditions: &[String], state: &str) -> bool {
    for condition in conditions {
        if condition == "others" {
            // WHEN OTHERS catches everything except QUERY_CANCELED
            // and ASSERT_FAILURE, matching PostgreSQL.
            if state != "57014" && state != "P0004" {
                return true;
            }
            continue;
        }
        let mapped = condition_sqlstate(condition)
            .map(ToString::to_string)
            .or_else(|| looks_like_sqlstate(condition).then(|| condition.to_ascii_uppercase()));
        let Some(mapped) = mapped else {
            continue;
        };
        if mapped == state {
            return true;
        }
        // Category codes (ending in 000) match their whole class.
        if mapped.ends_with("000") && state.get(..2) == mapped.get(..2) {
            return true;
        }
    }
    false
}

/// Substitute `%` placeholders in a RAISE format string.
fn format_raise_message(format: &str, args: &[Value]) -> Result<String, SQLError> {
    let mut out = String::with_capacity(format.len() + 16);
    let mut chars = format.chars().peekable();
    let mut next_arg = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let Some(value) = args.get(next_arg) else {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "too few parameters specified for RAISE".into(),
            });
        };
        next_arg += 1;
        out.push_str(&raise_text(value));
    }
    if next_arg < args.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "too many parameters specified for RAISE".into(),
        });
    }
    Ok(out)
}

/// Text form of a value inside a RAISE message (`NULL` renders as
/// `<NULL>`, booleans as `t` / `f`, arrays in brace form).
fn raise_text(value: &Value) -> String {
    match value {
        Value::Null => "<NULL>".into(),
        Value::Bool(b) => (if *b { "t" } else { "f" }).into(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Decimal(v) => v.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Bytes(b) => {
            use std::fmt::Write as _;
            let mut out = String::with_capacity(2 + b.len() * 2);
            out.push_str("\\x");
            for byte in b {
                let _ = write!(out, "{byte:02x}");
            }
            out
        }
        Value::List(items) => {
            let inner = items.iter().map(raise_text).collect::<Vec<_>>().join(",");
            format!("{{{inner}}}")
        }
        Value::Map(map) => {
            let inner = map.values().map(raise_text).collect::<Vec<_>>().join(",");
            format!("({inner})")
        }
    }
}

/// Resolver mapping names / `$n` references onto interpreter datums.
struct DatumResolver<'a> {
    datums: &'a [PLpgSQLDatum],
    values: &'a [Value],
    bindings: &'a HashMap<String, Vec<usize>>,
    error: Option<&'a (String, String)>,
    param_count: usize,
}

impl DatumResolver<'_> {
    fn lookup(&self, name: &str) -> Option<usize> {
        if let Some(stack) = self.bindings.get(name) {
            return stack.last().copied();
        }
        let lower = name.to_ascii_lowercase();
        if lower != name {
            if let Some(stack) = self.bindings.get(&lower) {
                return stack.last().copied();
            }
        }
        None
    }
}

impl VariableResolver for DatumResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<Value>, SQLError> {
        if let Some((state, message)) = self.error {
            if name.eq_ignore_ascii_case("sqlstate") {
                return Ok(Some(Value::Str(state.clone())));
            }
            if name.eq_ignore_ascii_case("sqlerrm") {
                return Ok(Some(Value::Str(message.clone())));
            }
        }
        Ok(self.lookup(name).map(|idx| self.values[idx].clone()))
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Value>, SQLError> {
        let Some(idx) = self.lookup(qualifier) else {
            return Ok(None);
        };
        match &self.datums[idx] {
            PLpgSQLDatum::Rec { name } => match &self.values[idx] {
                Value::Map(map) => {
                    let value = map
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(column))
                        .map(|(_, value)| value.clone());
                    match value {
                        Some(value) => Ok(Some(value)),
                        None => Err(SQLError::Routine {
                            sqlstate: "42703".into(),
                            message: format!("record \"{name}\" has no field \"{column}\""),
                        }),
                    }
                }
                Value::Null => Err(SQLError::Routine {
                    sqlstate: "55000".into(),
                    message: format!("record \"{name}\" is not assigned yet"),
                }),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<Value>, SQLError> {
        if index >= 1 && index <= self.param_count {
            Ok(Some(self.values[index - 1].clone()))
        } else {
            Ok(None)
        }
    }
}
