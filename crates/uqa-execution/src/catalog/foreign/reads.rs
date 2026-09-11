//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read guards shared by foreign catalog inquiries and definition execution.
use super::StoredForeignTable;
use crate::catalog::security::TableSecurity;
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;
pub type ForeignServersRead<'a> =
    Box<dyn Deref<Target = BTreeMap<String, uqa_fdw::ForeignServer>> + 'a>;
pub type ForeignTablesRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type ForeignSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, TableSecurity>> + 'a>;
pub trait ForeignRegistryReads {
    fn servers(&self) -> ForeignServersRead<'_>;
    fn tables(&self) -> ForeignTablesRead<'_>;
    fn security(&self) -> ForeignSecurityRead<'_>;
}
