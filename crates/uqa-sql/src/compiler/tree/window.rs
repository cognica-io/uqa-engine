//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window-frame and type-cast lowering.

use super::{compile_expr, extract_strings, Expr, NodeEnum, Result, SQLError};

pub(in crate::compiler) fn compile_window_frame(
    w: &pg_query::protobuf::WindowDef,
) -> Result<Option<crate::ast::WindowFrame>> {
    use crate::ast::{FrameBound, FrameMode, WindowFrame};
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
    if f & FRAMEOPTION_EXCLUSION != 0 {
        return Err(SQLError::Unsupported(
            "window frame EXCLUDE clauses are not represented by WindowFrame".into(),
        ));
    }
    // PostgreSQL always encodes a default frame in `frame_options`
    // (RANGE UNBOUNDED PRECEDING TO CURRENT ROW). Only honor the
    // frame when the user explicitly wrote one - that's exactly what
    // the `FRAMEOPTION_NONDEFAULT` bit indicates.
    if f & FRAMEOPTION_NONDEFAULT == 0 {
        if w.start_offset.is_some() || w.end_offset.is_some() {
            return Err(SQLError::Internal(
                "default window frame unexpectedly has an offset expression".into(),
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
    Ok(Some(WindowFrame { mode, start, end }))
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
    let raw_names = extract_strings(&type_name.names)?;
    // libpg_query reports built-in types qualified as `pg_catalog.<name>`;
    // peel the schema off so the evaluator only ever sees the bare type
    // and treat aliases (`int4` -> `integer`, `float8` -> `double
    // precision`) up front.
    let mut ty = raw_names
        .last()
        .ok_or_else(|| SQLError::Internal("TypeCast target has no name components".into()))?
        .to_lowercase();
    ty = match ty.as_str() {
        "int2" => "smallint".to_string(),
        "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "float4" => "real".to_string(),
        "float8" => "double precision".to_string(),
        _ => ty,
    };
    // Carry length / precision modifiers (`varchar(1)`, `numeric(10,2)`)
    // so the evaluator can truncate / rescale like PostgreSQL.
    if matches!(
        ty.as_str(),
        "varchar" | "bpchar" | "char" | "character" | "character varying" | "numeric" | "decimal"
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
    Ok(Expr::Cast {
        expr: Box::new(inner),
        ty,
    })
}
