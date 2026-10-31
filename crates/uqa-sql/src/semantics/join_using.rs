//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema binding and positional output shaping for `PostgreSQL` qualified joins.

use uqa_core::memory::ProductionControl;

mod production;
pub use production::{
    join_using_column_names_with_control, join_using_predicate_owned,
    resolve_join_using_with_control,
};

use crate::ast::{ColumnType, JoinKind, JoinUsing};
use crate::schema::join_output::JoinOutputSource;
use crate::SQLError;
use crate::{ColumnIdentity, RowSchema, ScalarExpr};

#[derive(Debug, Clone)]
pub struct ResolvedJoinUsing {
    columns: Vec<ResolvedJoinColumn>,
    alias: Option<String>,
}

pub type JoinUsingLayout = (
    Vec<(String, ColumnIdentity, JoinOutputSource)>,
    Vec<(ColumnIdentity, JoinOutputSource)>,
);

#[derive(Debug, Clone)]
struct ResolvedJoinColumn {
    name: String,
    left: usize,
    right: usize,
    comparison_type: Option<ColumnType>,
    output_type: Option<ColumnType>,
}

fn missing_using_column(column: &str, side: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!(
            "column \"{column}\" specified in USING clause does not exist in {side} table"
        ),
    }
}

fn ambiguous_using_column(column: &str, side: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42702".into(),
        message: format!("common column name \"{column}\" appears more than once in {side} table"),
    }
}

pub fn resolve_join_using(
    explicit: Option<&JoinUsing>,
    natural: bool,
    left: &RowSchema,
    right: &RowSchema,
) -> Result<Option<ResolvedJoinUsing>, SQLError> {
    resolve_join_using_with_control(
        explicit,
        natural,
        left,
        right,
        &ProductionControl::uncontrolled(),
    )
    .map(|using| {
        using.map(|using| {
            using
                .into_uncontrolled()
                .expect("ordinary USING resolution has no reservation")
        })
    })
}

impl ResolvedJoinUsing {
    /// Borrow the physical inputs consumed by the USING equality predicates without constructing their scalar trees. Logical positions and NULL slots resolve through the original child layouts, with cancellation checked even when a pair contributes no physical input.
    pub fn visit_input_slots(
        &self,
        left: &RowSchema,
        right: &RowSchema,
        control: &ProductionControl<'_>,
        visitor: &mut impl FnMut(usize) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        for column in &self.columns {
            for slot in [
                left.slot(column.left),
                right
                    .slot(column.right)
                    .map(|slot| left.physical_width() + slot),
            ] {
                control.check()?;
                if let Some(slot) = slot {
                    visitor(slot)?;
                }
            }
        }
        Ok(())
    }
}

pub fn join_using_predicate(
    using: &ResolvedJoinUsing,
    left: &RowSchema,
    right: &RowSchema,
) -> Option<ScalarExpr> {
    let control = ProductionControl::uncontrolled();
    join_using_predicate_owned(using, left, right, &control)
        .expect("ordinary USING predicate")
        .map(|predicate| {
            predicate
                .finish(&control)
                .expect("ordinary USING scalar")
                .into_uncontrolled()
                .expect("ordinary USING scalar has no reservation")
        })
}

/// Bind the visible and hidden output identities of a qualified join without
/// constructing or executing the join. Static source-schema binding and the
/// physical operator share this layout so an empty or correlated right side
/// cannot lose its declared types.
pub fn join_using_output_schema(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<RowSchema, SQLError> {
    let input = RowSchema::join(left, right, std::iter::empty());
    let (columns, aliases) = join_using_layout(kind, left, right, using)?;
    crate::schema::join_output::compile_layout(&input, &columns, &aliases)
        .map(|(schema, _)| schema)
        .map_err(|error| SQLError::Internal(error.0))
}

/// Add a parenthesized JOIN alias to the input-side schemas for null-rejection analysis. Merged FULL JOIN columns belong to neither side exclusively because their value is a coalesce of both inputs.
pub fn join_alias_input_schemas(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: Option<&ResolvedJoinUsing>,
    alias: &str,
    column_aliases: &[String],
) -> Result<(RowSchema, RowSchema), SQLError> {
    join_alias_input_schemas_with_control(
        kind,
        left,
        right,
        using,
        alias,
        column_aliases,
        &ProductionControl::uncontrolled(),
    )
}

mod input_aliases;
pub use input_aliases::join_alias_input_schemas_with_control;

pub fn join_using_layout(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<JoinUsingLayout, SQLError> {
    let right_offset = left.len();
    let mut columns = Vec::with_capacity(left.len() + right.len() - using.columns.len());
    let mut merged_sources = Vec::with_capacity(using.columns.len());
    production::visit_column_order(
        kind,
        left.len(),
        right.len(),
        using,
        &ProductionControl::uncontrolled(),
        |column| {
            match column {
                production::OutputColumn::Merged(column) => {
                    let source = match kind {
                        JoinKind::Inner | JoinKind::Left => coerced_output_source(
                            column.left,
                            left.column_type(column.left),
                            column.output_type.as_ref(),
                        ),
                        JoinKind::Right => coerced_output_source(
                            right_offset + column.right,
                            right.column_type(column.right),
                            column.output_type.as_ref(),
                        ),
                        JoinKind::Full => column.output_type.as_ref().map_or(
                            JoinOutputSource::Input(column.left),
                            |ty| JoinOutputSource::Coalesce {
                                left: column.left,
                                right: right_offset + column.right,
                                ty: ty.clone(),
                            },
                        ),
                        JoinKind::Cross => {
                            unreachable!("qualified cross join rejected by column selection")
                        }
                    };
                    columns.push((
                        column.name.clone(),
                        ColumnIdentity::unqualified(&column.name),
                        source.clone(),
                    ));
                    merged_sources.push((column.name.clone(), source));
                }
                production::OutputColumn::Left(position) => columns.push((
                    left.public_name(position)
                        .unwrap_or(&left.columns()[position])
                        .to_owned(),
                    left.identities()[position].clone(),
                    JoinOutputSource::Input(position),
                )),
                production::OutputColumn::Right(position) => columns.push((
                    right
                        .public_name(position)
                        .unwrap_or(&right.columns()[position])
                        .to_owned(),
                    right.identities()[position].clone(),
                    JoinOutputSource::Input(right_offset + position),
                )),
            }
            Ok(())
        },
    )?;

    let mut aliases = Vec::new();
    for (position, identity) in left.identities().iter().enumerate() {
        if identity.qualifier().is_some() {
            aliases.push((identity.clone(), JoinOutputSource::Input(position)));
        }
    }
    for (position, identity) in right.identities().iter().enumerate() {
        if identity.qualifier().is_some() {
            aliases.push((
                identity.clone(),
                JoinOutputSource::Input(right_offset + position),
            ));
        }
    }
    if let Some(alias) = using.alias.as_ref() {
        aliases.extend(
            merged_sources
                .into_iter()
                .map(|(column, source)| (ColumnIdentity::qualified(alias, column), source)),
        );
    }

    Ok((columns, aliases))
}

fn coerced_output_source(
    input: usize,
    source: Option<&ColumnType>,
    target: Option<&ColumnType>,
) -> JoinOutputSource {
    match (source, target) {
        (Some(source), Some(target)) if source != target => JoinOutputSource::Cast {
            input,
            ty: target.clone(),
        },
        _ => JoinOutputSource::Input(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Types;
    impl crate::FunctionTypeResolver for Types {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }
    }
    impl crate::routines::RoutineResolution for Types {}

    #[test]
    fn natural_columns_follow_left_input_order() {
        let left = RowSchema::with_qualified_types(
            "l",
            vec!["b".into(), "a".into(), "only".into()],
            vec![None; 3],
        );
        let right = RowSchema::with_qualified_types(
            "r",
            vec!["a".into(), "b".into(), "other".into()],
            vec![None; 3],
        );
        let resolved = resolve_join_using(None, true, &left, &right)
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn nested_using_predicate_preserves_resolved_input_positions() {
        let base = |alias: &str| {
            RowSchema::with_qualified_types(
                alias,
                vec!["payload".into(), "id".into()],
                vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
            )
        };
        let using = JoinUsing {
            columns: vec!["id".into()],
            alias: None,
        };
        let nested = |a: &str, b: &str, kind| {
            let (a, b) = (base(a), base(b));
            let resolved = resolve_join_using(Some(&using), false, &a, &b)
                .unwrap()
                .unwrap();
            join_using_output_schema(kind, &a, &b, &resolved).unwrap()
        };
        for kind in [
            JoinKind::Inner,
            JoinKind::Left,
            JoinKind::Right,
            JoinKind::Full,
        ] {
            let left = nested("a", "b", kind);
            let right = nested("c", "d", kind);
            let resolved = resolve_join_using(Some(&using), false, &left, &right)
                .unwrap()
                .unwrap();
            let predicate = join_using_predicate(&resolved, &left, &right).unwrap();
            let joined = RowSchema::join(&left, &right, []);
            let mut slots = Vec::new();
            resolved
                .visit_input_slots(
                    &left,
                    &right,
                    &ProductionControl::uncontrolled(),
                    &mut |slot| {
                        slots.push(slot);
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(
                slots,
                [
                    left.physical_slot(0).unwrap(),
                    left.physical_width() + right.physical_slot(0).unwrap(),
                ]
            );
            let crate::plan::UnifiedPlan::Query(query) =
                crate::plan::UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
            else {
                panic!("query")
            };
            let crate::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
                panic!("block")
            };
            block.r#where = Some(predicate);
            crate::binding::validate_query_block_references(
                &Types,
                &block,
                &joined,
                &[],
                &crate::binding::fixture::empty_binding_context(),
                None,
            )
            .unwrap();
            assert_ne!(
                left.physical_slot(0),
                Some(0),
                "logical positions must not be physical offsets"
            );
        }
    }

    #[test]
    fn using_requires_one_column_on_each_side() {
        let left = RowSchema::with_identities(
            vec!["id".into(), "id".into()],
            vec![
                ColumnIdentity::qualified("l", "id"),
                ColumnIdentity::qualified("other", "id"),
            ],
            vec![None; 2],
        );
        let right = RowSchema::with_qualified_types("r", vec!["id".into()], vec![None]);
        let error = resolve_join_using(
            Some(&JoinUsing {
                columns: vec!["id".into()],
                alias: None,
            }),
            false,
            &left,
            &right,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42702"));
    }
}
