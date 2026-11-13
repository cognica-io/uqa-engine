//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, FromClause, Input, InputError, JoinUsing, JsonToken, Native, OperatorJoinRelations,
    Parts, Stored, TableFunction, Values,
};

stored_struct!(TableFunction {
    name: required,
    binding: default(None),
    output_name: required,
    relations: default(None),
    args: required,
    column_aliases: default(Vec::new()),
    column_types: default(Vec::new())
});
stored_struct!(OperatorJoinRelations {
    left: required,
    right: required
});
stored_struct!(JoinUsing {
    columns: required,
    alias: default(None)
});

impl Stored for FromClause {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (tag, object) = input.tag(token)?;
        if !object {
            return Err(invalid());
        }
        let token = input.next()?;
        let result = match tag.as_str() {
            "Table" => fields!(
                input,
                token,
                values,
                FromClause::Table {
                    name: required,
                    qualifier: required,
                    alias: optional,
                    column_aliases: default(Vec::new()),
                    bound_columns: default(None),
                    include_descendants: default(true)
                }
            ),
            "Join" => fields!(
                input,
                token,
                values,
                FromClause::Join {
                    comma: default(false),
                    left: required,
                    right: required,
                    kind: required,
                    on: optional,
                    using: default(None),
                    natural: default(false),
                    alias: default(None),
                    column_aliases: default(Vec::new()),
                    lateral: required
                }
            ),
            "Values" => fields!(
                input,
                token,
                values,
                FromClause::Values {
                    lateral: default(false),
                    rows: required,
                    alias: optional,
                    column_aliases: required,
                    internal_relation: default(None),
                    internal_column_types: default(Vec::new()),
                    internal_column_collations: default(Vec::new())
                }
            ),
            "Function" => fields!(
                input,
                token,
                values,
                FromClause::Function {
                    name: required,
                    binding: default(None),
                    output_name: required,
                    relations: default(None),
                    args: required,
                    alias: optional,
                    column_aliases: required,
                    ordinality: default(false),
                    column_types: default(Vec::new())
                }
            ),
            "FunctionGroup" => fields!(
                input,
                token,
                values,
                FromClause::FunctionGroup {
                    functions: required,
                    alias: optional,
                    column_aliases: required,
                    ordinality: default(false)
                }
            ),
            "Subquery" => fields!(
                input,
                token,
                values,
                FromClause::Subquery {
                    lateral: default(false),
                    body: required,
                    alias: optional,
                    column_aliases: required
                }
            ),
            _ => Err(invalid()),
        }?;
        input.close_tag(object)?;
        Ok(result)
    }
}
