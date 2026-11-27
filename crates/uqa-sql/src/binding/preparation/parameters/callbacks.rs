//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal input providers retain their original borrowing and threading contracts.

use crate::{
    expr::{enums::EnumLabelCatalog, CatalogInputFunctions},
    schema::dependencies::oid_alias::OidAliasInput,
};

pub(in crate::binding::preparation) trait LiteralInput {
    fn aliases(&self) -> Option<&dyn OidAliasInput> {
        None
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        None
    }
    fn catalog_inputs(&self) -> Option<&dyn CatalogInputFunctions> {
        None
    }
}

/// Standalone expressions borrow the statement catalog across work windows. Each input function is borrowed only for its synchronous invocation.
impl LiteralInput for &dyn crate::routines::RoutineResolution {
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        crate::type_resolution::FunctionTypeResolver::enum_labels(*self)
    }
    fn catalog_inputs(&self) -> Option<&dyn CatalogInputFunctions> {
        self.catalog_input_functions()
    }
}

/// Fixed-parameter analysis has no external literal-input providers.
impl LiteralInput for () {}

#[derive(Default)]
pub(in crate::binding::preparation) struct InputCatalogs<'a> {
    pub(super) aliases: Option<&'a dyn OidAliasInput>,
    pub(super) enum_labels: Option<&'a dyn EnumLabelCatalog>,
    pub(super) catalog_inputs: Option<&'a dyn CatalogInputFunctions>,
}

impl LiteralInput for InputCatalogs<'_> {
    fn aliases(&self) -> Option<&dyn OidAliasInput> {
        self.aliases
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        self.enum_labels
    }
    fn catalog_inputs(&self) -> Option<&dyn CatalogInputFunctions> {
        self.catalog_inputs
    }
}
