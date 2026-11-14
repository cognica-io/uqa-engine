//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, shapes, Input, InputError, Kind, Pending, Shape, Value, Values};

impl<'a> Pending<'a> {
    pub(super) fn cte(&mut self, value: Value<'a>, values: &Values<'_>) -> Result<(), InputError> {
        let Shape::Fields(fields) = shapes::CTE_FIELDS else {
            unreachable!()
        };
        let mut input = Input::content_at_depth(value.text, values.control(), value.depth);
        let first = input.next()?;
        let mut seen = 0_u64;
        let mut body = None;
        input.object(first, |input, name, token| {
            if let Some(index) = fields.iter().position(|field| field.name == name) {
                let bit = 1 << index;
                if seen & bit != 0 {
                    return Err(invalid());
                }
                seen |= bit;
                return self.token(fields[index].shape, input, token);
            }
            let (text, depth) = input.encoded(token)?;
            if body.is_none() {
                let kind = match name {
                    "query" => Some(Kind::Select),
                    "insert" => Some(Kind::Insert),
                    "update" => Some(Kind::Update),
                    "delete" => Some(Kind::Delete),
                    "merge" => Some(Kind::Merge),
                    _ => None,
                };
                body = kind.map(|kind| (Shape::Node(kind), Value { text, depth }));
            }
            Ok(())
        })?;
        input.end()?;
        if seen & 7 != 7 {
            return Err(invalid());
        }
        let (shape, value) = body.ok_or_else(invalid)?;
        self.value(shape, value)
    }
}
