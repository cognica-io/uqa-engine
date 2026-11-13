//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, CteBody, CteCycleClause, CteSearchClause, FrameBound, FrameExclusion, Input,
    InputError, JsonToken, LockingClause, Native, OrderBy, Parts, Projection, SelectStmt, SetOp,
    Stored, Values, WindowCallModifiers, WindowDefinition, WindowFrame, WindowReference,
    WindowSpec, CTE,
};

stored_struct!(SelectStmt {
    windows: default(Vec::new()),
    projections: required,
    values: default(Vec::new()),
    from: optional,
    r#where: optional,
    group_by: required,
    grouping_sets: required,
    group_distinct: default(false),
    having: optional,
    order_by: required,
    limit: optional,
    with_ties: default(false),
    offset: optional,
    with: required,
    set_op: optional,
    distinct: required,
    distinct_on: required,
    locking: default(Vec::new())
});
stored_struct!(Projection {
    expr: required,
    alias: optional
});
stored_struct!(OrderBy {
    expr: required,
    descending: required,
    nulls: optional
});
stored_struct!(WindowDefinition {
    name: optional,
    inherited: optional,
    spec: required
});
stored_struct!(WindowFrame {
    mode: required,
    start: required,
    end: required,
    between: default(true),
    exclusion: default(FrameExclusion::NoOthers)
});
stored_struct!(WindowReference {
    name: required,
    kind: required
});
stored_struct!(WindowCallModifiers {
    distinct: required,
    ordered: required,
    within_group: required
});
stored_struct!(CteSearchClause {
    columns: required,
    breadth_first: required,
    sequence_column: required
});
stored_struct!(CteCycleClause {
    columns: required,
    mark_column: required,
    mark_value: required,
    mark_default: required,
    path_column: required
});
stored_struct!(SetOp {
    kind: required,
    all: required,
    left: default(None),
    right: required,
    combined_order_by: required,
    combined_limit: optional,
    combined_with_ties: default(false),
    combined_offset: optional
});
stored_struct!(LockingClause {
    strength: required,
    wait: required,
    relations: required
});

impl Stored for WindowSpec {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (mut definition, mut reference, mut partition_by, mut order_by, mut frame) =
            (None, None, None, None, None);
        let mut cursor = input.field_cursor(
            token,
            &[
                "definition",
                "reference",
                "partition_by",
                "order_by",
                "frame",
            ],
            [12, 28],
            false,
        )?;
        while let Some((index, token)) = cursor.next(input)? {
            match index {
                0 => definition = Some(input.stored(token, values)?),
                1 => reference = Some(input.stored(token, values)?),
                2 => partition_by = Some(input.stored(token, values)?),
                3 => order_by = Some(input.stored(token, values)?),
                _ => frame = Some(input.stored(token, values)?),
            }
        }
        let mut parts = Parts::new(input.control);
        let result = Self {
            raw_definition: None,
            definition: definition.and_then(|value| parts.take(value)),
            reference: reference.and_then(|value| parts.take(value)),
            partition_by: parts.take(partition_by.ok_or_else(invalid)?),
            order_by: parts.take(order_by.ok_or_else(invalid)?),
            frame: frame.and_then(|value| parts.take(value)),
        };
        parts.finish(result)
    }
}

impl Stored for FrameBound {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (tag, object) = input.tag(token)?;
        let mut parts = Parts::new(input.control);
        let value = match tag.as_str() {
            "Preceding" | "Following" if object => {
                let token = input.next()?;
                let value = parts.take(input.stored(token, values)?);
                if tag.as_str() == "Preceding" {
                    Self::Preceding(value)
                } else {
                    Self::Following(value)
                }
            }
            "UnboundedPreceding" | "UnboundedFollowing" | "CurrentRow" => {
                if object && input.next()? != JsonToken::Null {
                    return Err(invalid());
                }
                match tag.as_str() {
                    "UnboundedPreceding" => Self::UnboundedPreceding,
                    "UnboundedFollowing" => Self::UnboundedFollowing,
                    _ => Self::CurrentRow,
                }
            }
            _ => return Err(invalid()),
        };
        input.close_tag(object)?;
        parts.finish(value)
    }
}

impl Stored for CTE {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (mut name, mut columns, mut recursive, mut materialization, mut search, mut cycle) =
            (None, None, None, None, None, None);
        let mut body = None;
        let mut seen = 0u64;
        let mut cursor = input.object_cursor(token)?;
        while let Some((key, token)) = cursor.next(input)? {
            let key = key.as_str();
            let index = [
                "name",
                "columns",
                "recursive",
                "materialization",
                "search",
                "cycle",
            ]
            .iter()
            .position(|name| *name == key);
            if let Some(index) = index {
                let mask = 1 << index;
                if seen & mask != 0 {
                    return Err(invalid());
                }
                seen |= mask;
                match index {
                    0 => name = Some(input.stored(token, values)?),
                    1 => columns = Some(input.stored(token, values)?),
                    2 => recursive = Some(input.stored(token, values)?),
                    3 => materialization = Some(input.stored(token, values)?),
                    4 => search = Some(input.stored(token, values)?),
                    _ => cycle = Some(input.stored(token, values)?),
                }
            } else {
                let (text, depth) = input.encoded(token)?;
                if body.is_none() {
                    body = ["query", "insert", "update", "delete", "merge"]
                        .iter()
                        .position(|tag| *tag == key)
                        .map(|kind| (kind, text, depth));
                }
            }
        }
        if seen & 7 != 7 {
            return Err(invalid());
        }
        let (kind, text, depth) = body.ok_or_else(invalid)?;
        let mut body_input = Input::content_at_depth(text, input.control, depth);
        let token = body_input.next()?;
        let mut parts = Parts::new(input.control);
        let body = match kind {
            0 => CteBody::Query(parts.take(body_input.stored(token, values)?)),
            1 => CteBody::Insert(parts.take(body_input.stored(token, values)?)),
            2 => CteBody::Update(parts.take(body_input.stored(token, values)?)),
            3 => CteBody::Delete(parts.take(body_input.stored(token, values)?)),
            _ => CteBody::Merge(parts.take(body_input.stored(token, values)?)),
        };
        body_input.end()?;
        let value = Self {
            name: parts.take(name.ok_or_else(invalid)?),
            columns: parts.take(columns.ok_or_else(invalid)?),
            recursive: parts.take(recursive.ok_or_else(invalid)?),
            materialization: materialization
                .map(|value| parts.take(value))
                .unwrap_or_default(),
            search: search.and_then(|value| parts.take(value)),
            cycle: cycle.and_then(|value| parts.take(value)),
            body,
        };
        parts.finish(value)
    }
}
