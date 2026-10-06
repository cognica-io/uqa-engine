//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window-frame and type-cast lowering.

use super::{
    compile_expr, compile_named_window_spec, extract_strings, Expr, FromClause, Node, NodeEnum,
    Result, SQLError, SelectStmt, WindowReferenceKind, WindowSpec,
};

use crate::ast::{WindowDefinition, WindowDefinitionSyntax};

#[derive(Default)]
pub(in crate::compiler) struct NamedWindows {
    definitions: Vec<WindowDefinition>,
    syntax: Vec<WindowDefinitionSyntax>,
    resolved: Vec<WindowSpec>,
}

impl NamedWindows {
    fn named(&self, name: &str) -> Result<usize> {
        self.definitions
            .iter()
            .position(|definition| definition.name.as_deref() == Some(name))
            .ok_or_else(|| window_error("42704", format!("window \"{name}\" does not exist")))
    }

    fn append(
        &mut self,
        name: Option<String>,
        mut spec: WindowSpec,
        site: WindowSpecSite,
    ) -> Result<usize> {
        if spec
            .expressions_mut()
            .any(|expression| expression.contains_window())
        {
            return Err(window_error(
                "42P20",
                "window functions are not allowed in window definitions".into(),
            ));
        }
        let syntax = spec.raw_definition.take().ok_or_else(|| {
            SQLError::Internal("compiled window is missing its raw syntax".into())
        })?;
        let inherited = spec
            .reference
            .as_ref()
            .map(|reference| self.named(&reference.name))
            .transpose()?;
        let mut resolved = spec.clone();
        resolve_window_reference(&mut resolved, self, site)?;
        spec.reference = None;
        spec.definition = None;
        let slot = self.definitions.len();
        self.definitions.push(WindowDefinition {
            name,
            inherited,
            spec,
        });
        self.syntax.push(syntax);
        self.resolved.push(resolved);
        Ok(slot)
    }
}

/// Match `PostgreSQL`'s raw `WindowDef` comparison before lowering erases explicit sort spellings.
pub(super) fn raw_definition(definition: &pg_query::protobuf::WindowDef) -> WindowDefinitionSyntax {
    let mut key = serde_json::json!({
        "refname": definition.refname,
        "partition": definition.partition_clause,
        "order": definition.order_clause,
        "frame": definition.frame_options,
        "start": definition.start_offset,
        "end": definition.end_offset,
    });
    remove_source_locations(&mut key);
    WindowDefinitionSyntax(key)
}

fn remove_source_locations(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.remove("location");
            fields.remove("stmt_location");
            fields.remove("stmt_len");
            for value in fields.values_mut() {
                remove_source_locations(value);
            }
        }
        serde_json::Value::Array(items) => {
            for value in items {
                remove_source_locations(value);
            }
        }
        _ => {}
    }
}

pub(in crate::compiler) fn compile_named_windows(nodes: &[Node]) -> Result<NamedWindows> {
    let mut windows = NamedWindows::default();
    for node in nodes {
        let Some(NodeEnum::WindowDef(definition)) = node.node.as_ref() else {
            return Err(SQLError::Internal(format!(
                "WINDOW clause expected WindowDef, got {:?}",
                node.node
            )));
        };
        if definition.name.is_empty() {
            return Err(SQLError::Internal(
                "WINDOW clause definition has an empty name".into(),
            ));
        }
        if windows
            .definitions
            .iter()
            .any(|window| window.name.as_deref() == Some(&definition.name))
        {
            return Err(window_error(
                "42P20",
                format!("window \"{}\" is already defined", definition.name),
            ));
        }
        windows.append(
            Some(definition.name.clone()),
            compile_named_window_spec(definition)?,
            WindowSpecSite::WindowClause,
        )?;
    }
    Ok(windows)
}

pub(in crate::compiler) fn resolve_named_windows_in_expr(
    expr: &mut Expr,
    windows: &mut NamedWindows,
) -> Result<()> {
    match expr {
        Expr::Default
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            resolve_named_windows_in_exprs(args, windows)?;
            for order in order_by {
                resolve_named_windows_in_expr(&mut order.expr, windows)?;
            }
            if let Some(filter) = filter {
                resolve_named_windows_in_expr(filter, windows)?;
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            resolve_named_windows_in_exprs(items, windows)?;
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_named_windows_in_expr(lhs, windows)?;
            resolve_named_windows_in_expr(rhs, windows)?;
        }
        Expr::UnaryMinus(inner) | Expr::Not(inner) | Expr::Cast { expr: inner, .. } => {
            resolve_named_windows_in_expr(inner, windows)?;
        }
        Expr::IsNull { expr, .. } => resolve_named_windows_in_expr(expr, windows)?,
        Expr::Between { expr, low, high } => {
            resolve_named_windows_in_expr(expr, windows)?;
            resolve_named_windows_in_expr(low, windows)?;
            resolve_named_windows_in_expr(high, windows)?;
        }
        Expr::InList { expr, list, .. } => {
            resolve_named_windows_in_expr(expr, windows)?;
            resolve_named_windows_in_exprs(list, windows)?;
        }
        Expr::WindowCall {
            args, spec, filter, ..
        } => {
            resolve_named_windows_in_exprs(args, windows)?;
            if let Some(filter) = filter {
                resolve_named_windows_in_expr(filter, windows)?;
            }
            resolve_window_spec(spec, windows)?;
            resolve_window_spec_expressions(spec, windows)?;
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                resolve_named_windows_in_expr(base, windows)?;
            }
            for (condition, result) in when {
                resolve_named_windows_in_expr(condition, windows)?;
                resolve_named_windows_in_expr(result, windows)?;
            }
            if let Some(branch) = else_branch {
                resolve_named_windows_in_expr(branch, windows)?;
            }
        }
        Expr::InSubquery { expr, .. } => resolve_named_windows_in_expr(expr, windows)?,
    }
    Ok(())
}

pub(in crate::compiler) fn resolve_named_windows_in_select(
    select: &mut SelectStmt,
    windows: &mut NamedWindows,
) -> Result<()> {
    for projection in &mut select.projections {
        resolve_named_windows_in_expr(&mut projection.expr, windows)?;
    }
    for row in &mut select.values {
        resolve_named_windows_in_exprs(row, windows)?;
    }
    if let Some(from) = &mut select.from {
        resolve_named_windows_in_from(from, windows)?;
    }
    for expression in select
        .r#where
        .iter_mut()
        .chain(&mut select.group_by)
        .chain(select.grouping_sets.iter_mut().flatten())
        .chain(select.having.iter_mut())
        .chain(select.limit.iter_mut())
        .chain(select.offset.iter_mut())
        .chain(&mut select.distinct_on)
    {
        resolve_named_windows_in_expr(expression, windows)?;
    }
    for order in &mut select.order_by {
        resolve_named_windows_in_expr(&mut order.expr, windows)?;
    }
    if let Some(set_op) = &mut select.set_op {
        for order in &mut set_op.combined_order_by {
            resolve_named_windows_in_expr(&mut order.expr, windows)?;
        }
        for expression in set_op
            .combined_limit
            .iter_mut()
            .chain(set_op.combined_offset.iter_mut())
        {
            resolve_named_windows_in_expr(expression, windows)?;
        }
    }
    let mut index = 0;
    while index < windows.definitions.len() {
        let mut own = windows.definitions[index].spec.clone();
        resolve_window_spec_expressions(&mut own, windows)?;
        windows.definitions[index].spec = own;
        index += 1;
    }
    select.windows = std::mem::take(&mut windows.definitions);
    Ok(())
}

fn resolve_named_windows_in_from(from: &mut FromClause, windows: &mut NamedWindows) -> Result<()> {
    match from {
        FromClause::Table { .. } | FromClause::Subquery { .. } => {}
        FromClause::Join {
            left, right, on, ..
        } => {
            resolve_named_windows_in_from(left, windows)?;
            resolve_named_windows_in_from(right, windows)?;
            if let Some(on) = on {
                resolve_named_windows_in_expr(on, windows)?;
            }
        }
        FromClause::Values { rows, .. } => {
            for row in rows {
                resolve_named_windows_in_exprs(row, windows)?;
            }
        }
        FromClause::Function { args, .. } => {
            resolve_named_windows_in_exprs(args, windows)?;
        }
        FromClause::FunctionGroup { functions, .. } => {
            for function in functions {
                resolve_named_windows_in_exprs(&mut function.args, windows)?;
            }
        }
    }
    Ok(())
}

fn resolve_named_windows_in_exprs(exprs: &mut [Expr], windows: &mut NamedWindows) -> Result<()> {
    for expr in exprs {
        resolve_named_windows_in_expr(expr, windows)?;
    }
    Ok(())
}

/// Where a window specification that names another window was written; `transformWindowDefinitions` words one of its errors differently for an `OVER` clause.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowSpecSite {
    WindowClause,
    OverClause,
}

/// Apply a window reference as `transformWindowDefinitions` does, checking the `PARTITION BY`, `ORDER BY` and frame of the copy in that order.
fn resolve_window_spec(spec: &mut WindowSpec, windows: &mut NamedWindows) -> Result<()> {
    let Some(syntax) = spec.raw_definition.as_ref() else {
        return Ok(());
    };
    let slot = if let Some(reference) = spec
        .reference
        .as_ref()
        .filter(|reference| reference.kind == WindowReferenceKind::Direct)
    {
        windows.named(&reference.name)?
    } else if let Some(slot) = windows
        .syntax
        .iter()
        .position(|candidate| candidate == syntax)
    {
        slot
    } else {
        windows.append(None, spec.clone(), WindowSpecSite::OverClause)?
    };
    *spec = windows.resolved[slot].clone();
    spec.definition = Some(slot);
    Ok(())
}

fn resolve_window_reference(
    spec: &mut WindowSpec,
    windows: &NamedWindows,
    site: WindowSpecSite,
) -> Result<()> {
    let Some(reference) = spec.reference.take() else {
        return Ok(());
    };
    let base = &windows.resolved[windows.named(&reference.name)?];
    match reference.kind {
        WindowReferenceKind::Direct => {
            if !spec.partition_by.is_empty() || !spec.order_by.is_empty() || spec.frame.is_some() {
                return Err(SQLError::Internal(format!(
                    "direct window reference `{}` unexpectedly carries an inline definition",
                    reference.name
                )));
            }
            *spec = base.clone();
        }
        WindowReferenceKind::Copy => {
            if !spec.partition_by.is_empty() {
                return Err(window_error(
                    "42P20",
                    format!(
                        "cannot override PARTITION BY clause of window \"{}\"",
                        reference.name
                    ),
                ));
            }
            if !base.order_by.is_empty() && !spec.order_by.is_empty() {
                return Err(window_error(
                    "42P20",
                    format!(
                        "cannot override ORDER BY clause of window \"{}\"",
                        reference.name
                    ),
                ));
            }
            if base.frame.is_some() {
                let message = format!(
                    "cannot copy window \"{}\" because it has a frame clause",
                    reference.name
                );
                // `OVER (w)` alone copies nothing but the frame it cannot copy; `OVER w` uses the window as defined.
                return Err(
                    if site == WindowSpecSite::OverClause
                        && spec.order_by.is_empty()
                        && spec.frame.is_none()
                    {
                        SQLError::Diagnostic {
                            sqlstate: "42P20".into(),
                            message,
                            detail: None,
                            hint: Some("Omit the parentheses in this OVER clause.".into()),
                        }
                    } else {
                        window_error("42P20", message)
                    },
                );
            }
            spec.partition_by.clone_from(&base.partition_by);
            if spec.order_by.is_empty() {
                spec.order_by.clone_from(&base.order_by);
            }
        }
    }
    Ok(())
}

fn resolve_window_spec_expressions(
    spec: &mut WindowSpec,
    windows: &mut NamedWindows,
) -> Result<()> {
    resolve_named_windows_in_exprs(&mut spec.partition_by, windows)?;
    for order in &mut spec.order_by {
        resolve_named_windows_in_expr(&mut order.expr, windows)?;
    }
    if let Some(frame) = &mut spec.frame {
        for bound in [&mut frame.start, &mut frame.end] {
            match bound {
                crate::ast::FrameBound::Preceding(expr)
                | crate::ast::FrameBound::Following(expr) => {
                    resolve_named_windows_in_expr(expr, windows)?;
                }
                crate::ast::FrameBound::UnboundedPreceding
                | crate::ast::FrameBound::UnboundedFollowing
                | crate::ast::FrameBound::CurrentRow => {}
            }
        }
    }
    Ok(())
}

fn window_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(in crate::compiler) fn compile_window_frame(
    w: &pg_query::protobuf::WindowDef,
) -> Result<Option<crate::ast::WindowFrame>> {
    use crate::ast::{FrameBound, FrameExclusion, FrameMode, WindowFrame};
    // pg_query bit constants for frame_options.
    const FRAMEOPTION_NONDEFAULT: u32 = 0x000_0001;
    const FRAMEOPTION_RANGE: u32 = 0x000_0002;
    const FRAMEOPTION_ROWS: u32 = 0x000_0004;
    const FRAMEOPTION_GROUPS: u32 = 0x000_0008;
    const FRAMEOPTION_BETWEEN: u32 = 0x000_0010;
    const FRAMEOPTION_START_UNBOUNDED_PRECEDING: u32 = 0x000_0020;
    const FRAMEOPTION_END_UNBOUNDED_PRECEDING: u32 = 0x000_0040;
    const FRAMEOPTION_START_UNBOUNDED_FOLLOWING: u32 = 0x000_0080;
    const FRAMEOPTION_END_UNBOUNDED_FOLLOWING: u32 = 0x000_0100;
    const FRAMEOPTION_START_CURRENT_ROW: u32 = 0x000_0200;
    const FRAMEOPTION_END_CURRENT_ROW: u32 = 0x000_0400;
    const FRAMEOPTION_START_OFFSET_PRECEDING: u32 = 0x000_0800;
    const FRAMEOPTION_END_OFFSET_PRECEDING: u32 = 0x000_1000;
    const FRAMEOPTION_START_OFFSET_FOLLOWING: u32 = 0x000_2000;
    const FRAMEOPTION_END_OFFSET_FOLLOWING: u32 = 0x000_4000;
    const FRAMEOPTION_EXCLUDE_CURRENT_ROW: u32 = 0x000_8000;
    const FRAMEOPTION_EXCLUDE_GROUP: u32 = 0x001_0000;
    const FRAMEOPTION_EXCLUDE_TIES: u32 = 0x002_0000;
    const FRAMEOPTION_EXCLUSION: u32 =
        FRAMEOPTION_EXCLUDE_CURRENT_ROW | FRAMEOPTION_EXCLUDE_GROUP | FRAMEOPTION_EXCLUDE_TIES;
    const KNOWN_OPTIONS: u32 = FRAMEOPTION_NONDEFAULT
        | FRAMEOPTION_RANGE
        | FRAMEOPTION_ROWS
        | FRAMEOPTION_GROUPS
        | FRAMEOPTION_BETWEEN
        | FRAMEOPTION_START_UNBOUNDED_PRECEDING
        | FRAMEOPTION_END_UNBOUNDED_PRECEDING
        | FRAMEOPTION_START_UNBOUNDED_FOLLOWING
        | FRAMEOPTION_END_UNBOUNDED_FOLLOWING
        | FRAMEOPTION_START_CURRENT_ROW
        | FRAMEOPTION_END_CURRENT_ROW
        | FRAMEOPTION_START_OFFSET_PRECEDING
        | FRAMEOPTION_END_OFFSET_PRECEDING
        | FRAMEOPTION_START_OFFSET_FOLLOWING
        | FRAMEOPTION_END_OFFSET_FOLLOWING
        | FRAMEOPTION_EXCLUSION;
    let f = u32::try_from(w.frame_options).map_err(|_| {
        SQLError::Internal(format!(
            "window frame options cannot be negative: {}",
            w.frame_options
        ))
    })?;
    let unknown = f & !KNOWN_OPTIONS;
    if unknown != 0 {
        return Err(SQLError::Internal(format!(
            "window frame contains unknown option bits 0x{unknown:x}"
        )));
    }
    let exclusion = match f & FRAMEOPTION_EXCLUSION {
        0 => FrameExclusion::NoOthers,
        FRAMEOPTION_EXCLUDE_CURRENT_ROW => FrameExclusion::CurrentRow,
        FRAMEOPTION_EXCLUDE_GROUP => FrameExclusion::Group,
        FRAMEOPTION_EXCLUDE_TIES => FrameExclusion::Ties,
        other => {
            return Err(SQLError::Internal(format!(
                "window frame must select at most one exclusion, got bits 0x{other:x}"
            )));
        }
    };
    // PostgreSQL always encodes a default frame in `frame_options`
    // (RANGE UNBOUNDED PRECEDING TO CURRENT ROW). Only honor the
    // frame when the user explicitly wrote one - that's exactly what
    // the `FRAMEOPTION_NONDEFAULT` bit indicates.
    if f & FRAMEOPTION_NONDEFAULT == 0 {
        if w.start_offset.is_some()
            || w.end_offset.is_some()
            || exclusion != FrameExclusion::NoOthers
        {
            return Err(SQLError::Internal(
                "default window frame unexpectedly has an offset or exclusion".into(),
            ));
        }
        return Ok(None);
    }
    let mode_bits = f & (FRAMEOPTION_RANGE | FRAMEOPTION_ROWS | FRAMEOPTION_GROUPS);
    let mode = match mode_bits {
        FRAMEOPTION_RANGE => FrameMode::Range,
        FRAMEOPTION_ROWS => FrameMode::Rows,
        FRAMEOPTION_GROUPS => FrameMode::Groups,
        other => {
            return Err(SQLError::Internal(format!(
                "window frame must select exactly one mode, got bits 0x{other:x}"
            )));
        }
    };
    let start_bits = f
        & (FRAMEOPTION_START_UNBOUNDED_PRECEDING
            | FRAMEOPTION_START_UNBOUNDED_FOLLOWING
            | FRAMEOPTION_START_CURRENT_ROW
            | FRAMEOPTION_START_OFFSET_PRECEDING
            | FRAMEOPTION_START_OFFSET_FOLLOWING);
    if start_bits.count_ones() != 1 {
        return Err(SQLError::Internal(format!(
            "window frame must select exactly one start bound, got bits 0x{start_bits:x}"
        )));
    }
    let end_bits = f
        & (FRAMEOPTION_END_UNBOUNDED_PRECEDING
            | FRAMEOPTION_END_UNBOUNDED_FOLLOWING
            | FRAMEOPTION_END_CURRENT_ROW
            | FRAMEOPTION_END_OFFSET_PRECEDING
            | FRAMEOPTION_END_OFFSET_FOLLOWING);
    if end_bits.count_ones() != 1 {
        return Err(SQLError::Internal(format!(
            "window frame must select exactly one end bound, got bits 0x{end_bits:x}"
        )));
    }
    let start = if f & FRAMEOPTION_START_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_START_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_START_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_START_OFFSET_PRECEDING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_START_OFFSET_FOLLOWING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        return Err(SQLError::Internal(
            "window frame start bound was not recognized".into(),
        ));
    };
    let end = if f & FRAMEOPTION_END_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_END_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_END_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_END_OFFSET_PRECEDING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_END_OFFSET_FOLLOWING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        return Err(SQLError::Internal(
            "window frame end bound was not recognized".into(),
        ));
    };
    let start_uses_offset =
        f & (FRAMEOPTION_START_OFFSET_PRECEDING | FRAMEOPTION_START_OFFSET_FOLLOWING) != 0;
    if start_uses_offset != w.start_offset.is_some() {
        return Err(SQLError::Internal(
            "window frame start offset payload does not match its option bits".into(),
        ));
    }
    let end_uses_offset =
        f & (FRAMEOPTION_END_OFFSET_PRECEDING | FRAMEOPTION_END_OFFSET_FOLLOWING) != 0;
    if end_uses_offset != w.end_offset.is_some() {
        return Err(SQLError::Internal(
            "window frame end offset payload does not match its option bits".into(),
        ));
    }
    Ok(Some(WindowFrame {
        mode,
        start,
        end,
        between: f & FRAMEOPTION_BETWEEN != 0,
        exclusion,
    }))
}

pub(in crate::compiler) fn compile_type_cast(tc: &pg_query::protobuf::TypeCast) -> Result<Expr> {
    let arg = tc
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without arg".into()))?;
    let inner = compile_expr(arg)?;
    let type_name = tc
        .type_name
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without a target type".into()))?;
    let ty = compile_cast_type_name(type_name)?;
    // Input conversion belongs to ordered semantic analysis, after the owning
    // declaration's target, authority and preceding expressions are checked.
    Ok(Expr::Cast {
        implicit: false,
        expr: Box::new(inner),
        ty,
    })
}

fn compile_cast_type_name(type_name: &pg_query::protobuf::TypeName) -> Result<String> {
    let raw_names = extract_strings(&type_name.names)?;
    // libpg_query reports built-in types qualified as `pg_catalog.<name>`;
    // discard only that implicit qualification. Catalog-owned domains need
    // their schema retained through overload selection and runtime coercion.
    let mut names = raw_names;
    if names.first().is_some_and(|name| name == "pg_catalog") {
        names.remove(0);
    }
    if names.is_empty() {
        return Err(SQLError::Internal(
            "TypeCast target has no name components".into(),
        ));
    }
    let mut ty = names
        .iter()
        .map(|name| crate::compiler::render_relation_component(name))
        .collect::<Vec<_>>()
        .join(".");
    if names.len() == 1 && names[0] == "char" {
        ty = "\"char\"".to_string();
    }
    if crate::ast::ColumnType::from_sql_name(&ty).is_err()
        || (names.len() == 1
            && matches!(
                names[0].as_str(),
                "integer" | "smallint" | "bigint" | "boolean"
            ))
    {
        let mut declared = crate::compiler::types::compile_pg_type_reference(type_name, "cast")?;
        while let crate::ast::ColumnType::Array(element) = declared {
            declared = *element;
        }
        ty = declared.sql_name();
    }
    if names.len() == 1 {
        ty = match ty.as_str() {
            "int2" => "smallint".to_string(),
            "int4" => "integer".to_string(),
            "int8" => "bigint".to_string(),
            "float4" => "real".to_string(),
            "float8" => "double precision".to_string(),
            _ => ty,
        };
    }
    if ty == "interval" && !type_name.typmods.is_empty() {
        ty = crate::compiler::types::compile_pg_type_name(type_name, "cast")?.sql_name();
    }
    if matches!(ty.as_str(), "numeric" | "decimal") && !type_name.typmods.is_empty() {
        crate::compiler::types::compile_pg_type_name(type_name, "cast")?;
    }
    // Carry length / precision modifiers (`varchar(1)`, `numeric(10,2)`)
    // so the evaluator can truncate / rescale like PostgreSQL.
    if matches!(
        ty.as_str(),
        "varchar"
            | "bpchar"
            | "char"
            | "character"
            | "character varying"
            | "numeric"
            | "decimal"
            | "time"
            | "timetz"
            | "timestamp"
            | "timestamptz"
    ) {
        let mods = type_name
            .typmods
            .iter()
            .map(|node| match node.node.as_ref() {
                Some(NodeEnum::AConst(constant)) => match constant.val.as_ref() {
                    Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                        Ok(value.ival.to_string())
                    }
                    other => Err(SQLError::TypeMismatch(format!(
                        "type modifier must be an integer constant, got {other:?}"
                    ))),
                },
                other => Err(SQLError::TypeMismatch(format!(
                    "type modifier must be an integer constant, got {other:?}"
                ))),
            })
            .collect::<Result<Vec<_>>>()?;
        if !mods.is_empty() {
            ty = format!("{ty}({})", mods.join(","));
        }
    }
    if !type_name.array_bounds.is_empty() && !ty.ends_with("[]") {
        ty.push_str("[]");
    }
    Ok(ty)
}
