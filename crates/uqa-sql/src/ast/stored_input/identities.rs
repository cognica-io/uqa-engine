//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{number, Input, InputError, JsonToken};
use uqa_core::catalog_identity::CatalogObjectIdentity;

impl<'a> Input<'a, '_> {
    pub(crate) fn optional_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Option<[u8; 16]>, InputError> {
        if token == JsonToken::Null {
            Ok(None)
        } else {
            self.identity(token).map(Some)
        }
    }

    pub(crate) fn optional_catalog_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Option<CatalogObjectIdentity>, InputError> {
        if token == JsonToken::Null {
            Ok(None)
        } else {
            self.catalog_identity(token).map(Some)
        }
    }

    pub(crate) fn catalog_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<CatalogObjectIdentity, InputError> {
        let (mut object_id, mut oid) = ([0; 16], 0);
        self.closed_fields(
            token,
            &["object_id", "oid"],
            [3, 3],
            |input, index, token| {
                if index == 0 {
                    object_id = input.identity(token)?;
                } else {
                    oid = number(token)?;
                }
                Ok(())
            },
        )?;
        Ok(CatalogObjectIdentity { object_id, oid })
    }
}
