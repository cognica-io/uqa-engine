//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read guards shared by foreign catalog inquiries and definition execution.
use std::ops::Deref;
pub type ForeignWrappersRead<'a> =
    Box<dyn Deref<Target = uqa_sql::catalog::foreign_wrapper::ForeignWrappers> + 'a>;
pub type ForeignServersRead<'a> =
    Box<dyn Deref<Target = uqa_sql::catalog::foreign_server::ForeignServers> + 'a>;
pub type ForeignTablesRead<'a> =
    Box<dyn Deref<Target = crate::catalog::foreign::ForeignTables> + 'a>;
pub type ForeignSecurityRead<'a> = Box<
    dyn Deref<Target = uqa_sql::catalog::security::foreign_tables::ForeignTableSecurities> + 'a,
>;
pub type ForeignCatalogRead<'a> = Box<dyn Deref<Target = super::ForeignCatalog> + 'a>;
pub trait ForeignRegistryReads {
    fn relations(&self) -> ForeignCatalogRead<'_>;
    fn wrappers(&self) -> ForeignWrappersRead<'_>;
    fn servers(&self) -> ForeignServersRead<'_>;
    fn tables(&self) -> ForeignTablesRead<'_>;
    fn security(&self) -> ForeignSecurityRead<'_>;
}
