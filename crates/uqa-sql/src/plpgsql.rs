//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PL/pgSQL` function bodies: typed AST, parser, and the variable
//! binding rewriter.
//!
//! Bodies are parsed with `libpg_query`'s `PL/pgSQL` parser
//! (`pg_query::parse_plpgsql`), which returns the same JSON dump
//! `PostgreSQL` itself produces. This module lowers that JSON into a
//! typed AST whose embedded SQL fragments are pre-compiled into
//! [`Expr`] / [`Statement`] values, ready for execution against the
//! engine.
//!
//! Variable references inside embedded SQL are plain column
//! references after compilation. At execution time the interpreter
//! rewrites them into literals through [`VariableResolver`] /
//! [`bind_expr`] / [`bind_statement`] before handing the statement to
//! the engine. This matches `plpgsql.variable_conflict =
//! use_variable` resolution: a name that is both a `PL/pgSQL`
//! variable and a column of a queried table resolves to the variable
//! (stock `PostgreSQL` raises an ambiguity error instead).

use serde_json::Value as JSONValue;
use uqa_core::Value;

use crate::ast::{
    CreateFunction, Expr, FromClause, FunctionBody, FunctionParamMode, FunctionReturns, MergeWhen,
    Projection, SelectStmt, Statement, CTE,
};
use crate::error::{Result, SQLError};

// ---------------------------------------------------------------------
// Typed AST
// ---------------------------------------------------------------------

/// A parsed `PL/pgSQL` function body: the flat datum table plus the
/// outermost block.
#[derive(Debug, Clone)]
pub struct PLpgSQLFunction {
    pub datums: Vec<PLpgSQLDatum>,
    pub action: PLpgSQLBlock,
    /// Index of the implicit `FOUND` variable in [`Self::datums`].
    pub found_datum: Option<usize>,
}

impl PLpgSQLFunction {
    /// Datum indices used as `FOR i IN a..b` loop counters. The
    /// interpreter binds these names only while their loop runs so an
    /// outer variable with the same name stays visible elsewhere.
    pub fn fori_variable_datums(&self) -> std::collections::BTreeSet<usize> {
        let mut out = std::collections::BTreeSet::new();
        collect_fori_vars_block(&self.action, &mut out);
        out
    }

    /// Datum indices used as bound-cursor arguments. They are visible only
    /// while the cursor query is bound, not throughout the routine body.
    pub fn cursor_argument_datums(&self) -> std::collections::BTreeSet<usize> {
        let mut out = std::collections::BTreeSet::new();
        for datum in &self.datums {
            let PLpgSQLDatum::Var(var) = datum else {
                continue;
            };
            let Some(argument_row) = var.cursor.as_ref().and_then(|cursor| cursor.argument_row)
            else {
                continue;
            };
            if let Some(PLpgSQLDatum::Row { fields }) = self.datums.get(argument_row) {
                out.extend(fields.iter().map(|field| field.varno));
            }
        }
        out
    }
}

fn collect_fori_vars_block(block: &PLpgSQLBlock, out: &mut std::collections::BTreeSet<usize>) {
    collect_fori_vars_stmts(&block.body, out);
    for arm in &block.exceptions {
        collect_fori_vars_stmts(&arm.body, out);
    }
}

fn collect_fori_vars_stmts(stmts: &[PLpgSQLStmt], out: &mut std::collections::BTreeSet<usize>) {
    for stmt in stmts {
        match stmt {
            PLpgSQLStmt::Block(block) => collect_fori_vars_block(block, out),
            PLpgSQLStmt::If {
                then_body,
                elsifs,
                else_body,
                ..
            } => {
                collect_fori_vars_stmts(then_body, out);
                for (_, body) in elsifs {
                    collect_fori_vars_stmts(body, out);
                }
                if let Some(body) = else_body {
                    collect_fori_vars_stmts(body, out);
                }
            }
            PLpgSQLStmt::Case {
                arms, else_body, ..
            } => {
                for (_, body) in arms {
                    collect_fori_vars_stmts(body, out);
                }
                if let Some(body) = else_body {
                    collect_fori_vars_stmts(body, out);
                }
            }
            PLpgSQLStmt::Loop { body, .. } | PLpgSQLStmt::While { body, .. } => {
                collect_fori_vars_stmts(body, out);
            }
            PLpgSQLStmt::ForI { var, body, .. } => {
                out.insert(*var);
                collect_fori_vars_stmts(body, out);
            }
            PLpgSQLStmt::ForQuery { body, .. } => collect_fori_vars_stmts(body, out),
            _ => {}
        }
    }
}

/// One entry in the function's flat datum table. `varno` / `dno`
/// references inside statements index into this table.
#[derive(Debug, Clone)]
pub enum PLpgSQLDatum {
    Var(Box<PLpgSQLVar>),
    /// `RECORD` variable (also `FOR rec IN ...` loop targets).
    Rec {
        name: String,
    },
    /// `rec.field` assignment target.
    RecField {
        field: String,
        parent: usize,
    },
    /// Multi-variable target list (`SELECT ... INTO a, b`).
    Row {
        fields: Vec<PLpgSQLRowField>,
    },
}

impl PLpgSQLDatum {
    pub fn name(&self) -> Option<&str> {
        match self {
            PLpgSQLDatum::Var(v) => Some(&v.name),
            PLpgSQLDatum::Rec { name } => Some(name),
            PLpgSQLDatum::RecField { .. } | PLpgSQLDatum::Row { .. } => None,
        }
    }
}

/// Scalar `PL/pgSQL` variable (declared variable, parameter, loop
/// counter, or an internal compiler temporary).
#[derive(Debug, Clone)]
pub struct PLpgSQLVar {
    pub name: String,
    /// Normalized type name (`integer`, `text`, ...). Types the value
    /// layer cannot cast (e.g. `%type`, `record`) are kept verbatim
    /// and skipped at assignment time.
    pub type_name: String,
    pub default: Option<Expr>,
    pub constant: bool,
    pub not_null: bool,
    /// Definition of a bound cursor declared with `CURSOR (...) FOR query`.
    pub cursor: Option<PLpgSQLCursor>,
    /// Source line of the declaration; used to disambiguate loop
    /// variables that shadow outer names.
    pub lineno: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PLpgSQLCursor {
    pub query: Statement,
    pub argument_row: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct PLpgSQLCursorArgument {
    pub name: Option<String>,
    pub expr: Expr,
}

/// `name -> datum` slot of a row target.
#[derive(Debug, Clone)]
pub struct PLpgSQLRowField {
    pub name: String,
    pub varno: usize,
}

/// `[DECLARE ...] BEGIN ... [EXCEPTION ...] END` block.
#[derive(Debug, Clone)]
pub struct PLpgSQLBlock {
    pub label: Option<String>,
    pub body: Vec<PLpgSQLStmt>,
    pub exceptions: Vec<PLpgSQLExceptionArm>,
}

/// One `WHEN cond [OR cond ...] THEN stmts` arm of an exception
/// section.
#[derive(Debug, Clone)]
pub struct PLpgSQLExceptionArm {
    /// Lower-cased condition names (`others`, `division_by_zero`,
    /// ...). Explicit `SQLSTATE 'xxxxx'` conditions arrive as the
    /// five-character code.
    pub conditions: Vec<String>,
    pub body: Vec<PLpgSQLStmt>,
}

/// `RAISE` severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseLevel {
    Debug,
    Log,
    Info,
    Notice,
    Warning,
    Error,
}

impl RaiseLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RaiseLevel::Debug => "DEBUG",
            RaiseLevel::Log => "LOG",
            RaiseLevel::Info => "INFO",
            RaiseLevel::Notice => "NOTICE",
            RaiseLevel::Warning => "WARNING",
            RaiseLevel::Error => "ERROR",
        }
    }
}

/// Assignment / `INTO` target.
#[derive(Debug, Clone)]
pub enum IntoTarget {
    /// A `RECORD` variable receives the whole row.
    Rec(usize),
    /// Positional list of scalar targets.
    Row(Vec<PLpgSQLRowField>),
}

/// Executable `PL/pgSQL` statement.
#[derive(Debug, Clone)]
pub enum PLpgSQLStmt {
    Block(PLpgSQLBlock),
    /// `target := expr` (also `=`). `target` indexes the datum table.
    Assign {
        target: usize,
        expr: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<PLpgSQLStmt>,
        elsifs: Vec<(Expr, Vec<PLpgSQLStmt>)>,
        else_body: Option<Vec<PLpgSQLStmt>>,
    },
    /// CASE statement. Simple form carries `t_expr` + the temporary
    /// datum the compiler references from each rewritten WHEN.
    Case {
        t_expr: Option<Expr>,
        t_varno: Option<usize>,
        arms: Vec<(Expr, Vec<PLpgSQLStmt>)>,
        else_body: Option<Vec<PLpgSQLStmt>>,
    },
    Loop {
        label: Option<String>,
        body: Vec<PLpgSQLStmt>,
    },
    While {
        label: Option<String>,
        cond: Expr,
        body: Vec<PLpgSQLStmt>,
    },
    /// `FOR i IN [REVERSE] lower..upper [BY step] LOOP`.
    ForI {
        label: Option<String>,
        var: usize,
        lower: Expr,
        upper: Expr,
        step: Option<Expr>,
        reverse: bool,
        body: Vec<PLpgSQLStmt>,
    },
    /// `FOR target IN <query> LOOP`.
    ForQuery {
        label: Option<String>,
        target: IntoTarget,
        query: Statement,
        body: Vec<PLpgSQLStmt>,
    },
    /// `EXIT` (`is_exit`) or `CONTINUE`, optionally labelled and
    /// conditional (`WHEN cond`).
    Exit {
        is_exit: bool,
        label: Option<String>,
        cond: Option<Expr>,
    },
    Return {
        value: Option<PLpgSQLReturnValue>,
    },
    /// `RETURN NEXT [expr]` - bare form emits the current OUT /
    /// TABLE column values.
    ReturnNext {
        value: Option<PLpgSQLReturnValue>,
    },
    ReturnQuery {
        query: Statement,
    },
    ReturnQueryExecute {
        query: Expr,
        params: Vec<Expr>,
    },
    Raise {
        level: RaiseLevel,
        condition: Option<String>,
        message: Option<String>,
        params: Vec<Expr>,
    },
    /// Embedded SQL statement, optionally `INTO [STRICT] target`.
    ExecSQL {
        stmt: Statement,
        into: Option<IntoTarget>,
        strict: bool,
    },
    /// `EXECUTE <string> [INTO [STRICT] target] [USING params]`.
    DynExecute {
        query: Expr,
        params: Vec<Expr>,
        into: Option<IntoTarget>,
        strict: bool,
    },
    Perform {
        query: Statement,
    },
    OpenCursor {
        cursor: usize,
        arguments: Vec<PLpgSQLCursorArgument>,
    },
    FetchCursor {
        cursor: usize,
        target: IntoTarget,
        direction: i64,
        count: i64,
    },
    CloseCursor {
        cursor: usize,
    },
    /// `GET DIAGNOSTICS var = KIND [, ...]` as `(kind, target datum)`.
    GetDiagnostics {
        items: Vec<(String, usize)>,
    },
}

/// Value source for `RETURN` and `RETURN NEXT`. `PostgreSQL` 18 stores a simple
/// datum reference in `retvarno`, distinct from a general SQL expression.
#[derive(Debug, Clone)]
pub enum PLpgSQLReturnValue {
    Expr(Expr),
    Datum(usize),
}

// ---------------------------------------------------------------------
// Parsing: definition -> canonical text -> libpg_query JSON -> AST
// ---------------------------------------------------------------------

/// Parse the `PL/pgSQL` body of a stored definition. The definition
/// is re-serialized into a canonical `CREATE FUNCTION` statement so
/// restore-from-catalog and fresh DDL take the same path.
mod binding;
mod conditions;
mod json_validation;
mod lowering_expression;
mod lowering_statement;
mod parsing;

use json_validation::{
    ensure_single_tag, expect_tag, json_bool_or_false, json_i64_or_zero, json_kind,
    json_optional_i64, json_optional_str, json_optional_usize, json_usize_or_zero,
    normalize_plpgsql_type, optional_array, require, require_i64, require_nonempty_str,
    validate_assignable_datum, validate_record_datum, validate_scalar_datum,
};
use lowering_expression::{lower_expr, lower_expr_list, lower_full_statement};
use lowering_statement::lower_block;
use parsing::{lower_row_fields, normalize_condition};

pub use binding::{bind_expr, bind_select, bind_statement, VariableResolver};
pub use conditions::{condition_sqlstate, condition_sqlstates};
pub use lowering_expression::compile_expression_text;
pub use parsing::{parse_do_block, parse_function};

#[cfg(test)]
mod tests;
