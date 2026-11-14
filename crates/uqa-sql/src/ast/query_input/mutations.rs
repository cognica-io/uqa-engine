//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, DeleteStmt, Input, InputError, InsertStmt, JsonToken, MergeStmt,
    MergeTargetColumnBinding, MergeWhen, Native, OnConflict, OnConflictAction, Parts,
    ReturningAliases, Stored, UpdateStmt, Values,
};
use uqa_core::memory::{Budgeted, OwnedMap};

stored_struct!(InsertStmt {
    table: required,
    target_relation_bound: default(false),
    target_qualifier: required,
    target_alias: default(None),
    include_descendants: default(true),
    columns: required,
    overriding: default(None),
    with: required,
    rows: required,
    select_source: optional,
    on_conflict: optional,
    returning: required,
    returning_aliases: required
});
stored_struct!(UpdateStmt {
    table: required,
    target_relation_bound: default(false),
    target_qualifier: required,
    target_alias: default(None),
    include_descendants: default(true),
    assignments: required,
    r#where: optional,
    with: required,
    from: optional,
    returning: required,
    returning_aliases: required
});
stored_struct!(DeleteStmt {
    table: required,
    target_relation_bound: default(false),
    target_qualifier: required,
    target_alias: default(None),
    include_descendants: default(true),
    r#where: optional,
    with: required,
    using: optional,
    returning: required,
    returning_aliases: required
});
stored_struct!(MergeStmt {
    with: default(Vec::new()),
    target: required,
    target_qualifier: required,
    target_alias: optional,
    target_column_bindings: default(OwnedMap::new()),
    include_descendants: default(true),
    source: required,
    join_condition: required,
    when_clauses: required,
    returning: required,
    returning_aliases: required
});
stored_struct!(ReturningAliases {
    old: required,
    new: required,
    old_explicit: default(false),
    new_explicit: default(false)
});
stored_struct!(OnConflict {
    predicate: default(None),
    constraint: default(None),
    conflict_columns: required,
    expressions: default(Vec::new()),
    action: required
});

stored_enum!(OnConflictAction { Nothing, Update { assignments: required, r#where: optional } });

stored_enum!(MergeWhen {
    UpdateMatched { condition: optional,
                    assignments: required },
    UpdateNotMatchedBySource { condition: optional,
                    assignments: required },
    DeleteMatched { condition: optional },
    DeleteNotMatchedBySource { condition: optional },
    InsertNotMatched { condition: optional,
                    columns: required,
                    overriding: default(None),
                    values: required },
    NothingMatched { condition: optional },
    NothingNotMatched { condition: optional },
    NothingNotMatchedBySource { condition: optional },
});

impl Stored for MergeTargetColumnBinding {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut object_id = None;
        let mut attribute_number = None;
        let mut domain_dependencies = None;
        let mut cursor = input.field_cursor(
            token,
            &["object_id", "attribute_number", "domain_dependencies"],
            [1, 1],
            false,
        )?;
        while let Some((index, token)) = cursor.next(input)? {
            match index {
                0 => object_id = Some(input.identity(token)?),
                1 => attribute_number = Some(input.stored(token, values)?),
                _ => {
                    domain_dependencies = Some(input.set(token, |_, token| {
                        super::super::stored_input::number::<u32>(token)
                    })?);
                }
            }
        }
        let mut parts = Parts::new(input.control);
        let value = Self {
            object_id: object_id.ok_or_else(invalid)?,
            attribute_number: attribute_number.and_then(|value| parts.take(value)),
            domain_dependencies: domain_dependencies
                .map(|value| parts.take(value))
                .unwrap_or_default(),
        };
        parts.finish(value)
    }
}

impl Stored for OwnedMap<String, MergeTargetColumnBinding> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let control = input.control;
        let mut result = Self::new();
        let mut cursor = input.object_cursor(token)?;
        while let Some((name, token)) = cursor.next(input)? {
            let name = name.as_str();
            let (key, key_memory) = input.control.copy_text(name)?.into_parts();
            let (value, value_memory) = input
                .stored::<MergeTargetColumnBinding>(token, values)?
                .into_parts();
            let memory = control
                .combine(key_memory, value_memory)
                .expect("query input carries an allocation allowance");
            // Each native map node owns its complete payload. Overwritten values retire before the replacement node is published in this private candidate.
            drop(result.remove(name));
            result.insert_admitted_key(Budgeted::new(key, memory), value)?;
        }
        Native::new(
            control.finish(result, control.empty_reservation())?,
            control,
        )
    }
}
