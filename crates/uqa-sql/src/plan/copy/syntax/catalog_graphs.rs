//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph label copies admit every owned constraint name and ordered map node.

use super::{Copying, Result, SyntaxCopy};
use crate::catalog::graph_oids::LabelCatalogOids;

impl SyntaxCopy for LabelCatalogOids {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        let Self {
            sequence,
            relation,
            id_default,
            properties_default,
            not_null,
            toast_table,
            toast_index,
            primary_key,
            endpoint_indexes,
            trigger,
        } = self;
        Ok(Self {
            sequence: *sequence,
            relation: *relation,
            id_default: *id_default,
            properties_default: *properties_default,
            not_null: not_null.copy_syntax(copying)?,
            toast_table: *toast_table,
            toast_index: *toast_index,
            primary_key: *primary_key,
            endpoint_indexes: *endpoint_indexes,
            trigger: *trigger,
        })
    }
}
