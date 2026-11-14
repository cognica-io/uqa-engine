//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced};
use crate::ast::{
    stored_input::{boolean, unit},
    ForeignKey, ForeignKeyAction, ForeignKeyMatch,
};

impl<'a> Input<'a, '_> {
    pub(super) fn table_foreign_key(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<ForeignKey>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = ForeignKey {
            referenced_index: None,
            referenced_key: None,
            name: None,
            object_id: None,
            catalog_identity: None,
            local_columns: Vec::new(),
            ref_table: String::new(),
            ref_columns: Vec::new(),
            on_update: ForeignKeyAction::NoAction,
            on_delete: ForeignKeyAction::NoAction,
            on_delete_set_columns: Vec::new(),
            match_type: ForeignKeyMatch::Simple,
            enforced: true,
            validated: true,
            deferrable: false,
            initially_deferred: false,
            period: false,
            referenced_partitions: Vec::new(),
        };
        self.fields_with_defaults(
            token,
            &[
                "referenced_index",
                "referenced_key",
                "name",
                "object_id",
                "catalog_identity",
                "local_columns",
                "ref_table",
                "ref_columns",
                "on_update",
                "on_delete",
                "on_delete_set_columns",
                "match_type",
                "enforced",
                "validated",
                "deferrable",
                "initially_deferred",
                "period",
                "referenced_partitions",
            ],
            224,
            228,
            |input, index, token| {
                match index {
                    0 => value.referenced_index = input.optional_identity(token)?,
                    1 => value.referenced_key = parts.take(input.optional_text(token)?),
                    2 => value.name = parts.take(input.optional_text(token)?),
                    3 => value.object_id = input.optional_identity(token)?,
                    4 => value.catalog_identity = input.optional_catalog_identity(token)?,
                    5 => value.local_columns = parts.take(input.texts(token)?),
                    6 => value.ref_table = parts.take(input.text(token)?),
                    7 => value.ref_columns = parts.take(input.texts(token)?),
                    8 => value.on_update = unit(&input.unit_tag(token)?)?,
                    9 => value.on_delete = unit(&input.unit_tag(token)?)?,
                    10 => value.on_delete_set_columns = parts.take(input.texts(token)?),
                    11 => value.match_type = unit(&input.unit_tag(token)?)?,
                    12 => value.enforced = boolean(token)?,
                    13 => value.validated = boolean(token)?,
                    14 => value.deferrable = boolean(token)?,
                    15 => value.initially_deferred = boolean(token)?,
                    16 => value.period = boolean(token)?,
                    _ => {
                        value.referenced_partitions =
                            parts.take(input.array(token, Self::referenced_partition)?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
