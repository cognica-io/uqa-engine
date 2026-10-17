//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected native input keys reuse exact SQL framing and borrowed array traversal.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{
    catalog::index::operator_classes::BoundBTreeOperatorClass,
    expr::{with_btree_input, BTreeInput, SQLValueCatalog},
    SQLError,
};

/// Exact input keys within one selected class and field-identity domain, when the selected collation preserves byte equality. A nondeterministic collation must supply its own equality normalization before this boundary. SQL owns input interpretation; this does not invoke input, domain checks or range canonicalization.
pub fn btree_input_binary_key_with_catalog_and_control(
    binding: BoundBTreeOperatorClass<'_>,
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    with_btree_input(binding, value, catalog, control, |input| match input {
        BTreeInput::Value(value, ty) => super::canonical_typed_row_key_with_catalog_and_control(
            std::iter::once((Some(value), Some(ty))),
            catalog,
            control,
        ),
        BTreeInput::RecordImage(value) => {
            super::record_image_key_with_catalog_and_control(value, catalog, control)
        }
        BTreeInput::Array(array) => {
            let mut output = super::CatalogKeyOutput {
                values: ProductionVec::new(*control),
                enums: catalog,
                sql_hash: true,
            };
            let result = (|| {
                super::encode_len(1, &mut output)?;
                super::encode_array_with_control(array, &mut output, control)
            })();
            result.map_err(super::sql_error)?;
            Ok(output.values.finish()?)
        }
    })
}

#[cfg(test)]
mod tests;
