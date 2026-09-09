//! Catalog snapshots for PostgreSQL's PL/pgSQL type lookup callbacks.

use std::collections::BTreeMap;
use std::ffi::{c_void, CStr, CString};

use crate::bindings::*;
use crate::{Error, Result};

/// The pg_type attributes needed to distinguish scalar, domain, array, and composite declarations.
#[derive(Debug, Clone)]
pub struct PlpgsqlType {
    pub oid: u32,
    pub namespace_oid: u32,
    pub name: String,
    pub length: i16,
    pub by_value: bool,
    pub type_kind: u8,
    pub category: u8,
    pub alignment: u8,
    pub storage: u8,
    pub array_oid: u32,
    pub element_oid: u32,
    pub base_type_oid: u32,
    pub collation_oid: u32,
    pub subscript_handler_oid: u32,
}

/// An immutable catalog snapshot for one parse. `search_path` is the caller's effective namespace order, including pg_catalog where appropriate. Built-in types remain available through PostgreSQL's own fallback catalog.
#[derive(Debug, Clone, Default)]
pub struct PlpgsqlCatalog {
    pub namespaces: BTreeMap<String, u32>,
    pub search_path: Vec<String>,
    pub types: Vec<PlpgsqlType>,
}

struct CatalogContext<'a> {
    catalog: &'a PlpgsqlCatalog,
    names: Vec<CString>,
}

impl CatalogContext<'_> {
    fn write_type(&self, index: usize, output: *mut PgQueryPlpgsqlTypeMetadata) {
        let ty = &self.catalog.types[index];
        let value = PgQueryPlpgsqlTypeMetadata {
            oid: ty.oid,
            namespace_oid: ty.namespace_oid,
            name: self.names[index].as_ptr(),
            length: ty.length,
            by_value: ty.by_value,
            type_kind: ty.type_kind as _,
            category: ty.category as _,
            alignment: ty.alignment as _,
            storage: ty.storage as _,
            array_oid: ty.array_oid,
            element_oid: ty.element_oid,
            base_type_oid: ty.base_type_oid,
            collation_oid: ty.collation_oid,
            subscript_handler_oid: ty.subscript_handler_oid,
        };
        // The C parser supplies a valid output pointer and copies this metadata before the next callback.
        unsafe { output.write(value) };
    }
}

unsafe extern "C" fn lookup_namespace(
    context: *mut c_void,
    name: *const std::os::raw::c_char,
    output: *mut u32,
) -> PgQueryCatalogLookupResult {
    let context = &*(context as *const CatalogContext<'_>);
    let name = CStr::from_ptr(name).to_bytes();
    match context
        .catalog
        .namespaces
        .iter()
        .find(|(candidate, _)| candidate.as_bytes() == name)
    {
        Some((_, oid)) => {
            output.write(*oid);
            PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_FOUND
        }
        None => PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_NOT_FOUND,
    }
}

unsafe extern "C" fn lookup_type_by_name(
    context: *mut c_void,
    schema: *const std::os::raw::c_char,
    name: *const std::os::raw::c_char,
    output: *mut PgQueryPlpgsqlTypeMetadata,
) -> PgQueryCatalogLookupResult {
    let context = &*(context as *const CatalogContext<'_>);
    let name = CStr::from_ptr(name).to_bytes();
    let in_schema = |schema: &[u8]| {
        let namespace = context
            .catalog
            .namespaces
            .iter()
            .find(|(candidate, _)| candidate.as_bytes() == schema)
            .map(|(_, oid)| *oid)?;
        context
            .catalog
            .types
            .iter()
            .position(|ty| ty.namespace_oid == namespace && ty.name.as_bytes() == name)
    };
    let found = if schema.is_null() {
        context
            .catalog
            .search_path
            .iter()
            .find_map(|schema| in_schema(schema.as_bytes()))
    } else {
        in_schema(CStr::from_ptr(schema).to_bytes())
    };
    match found {
        Some(index) => {
            context.write_type(index, output);
            PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_FOUND
        }
        None => PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_NOT_FOUND,
    }
}

unsafe extern "C" fn lookup_type_by_oid(
    context: *mut c_void,
    oid: u32,
    output: *mut PgQueryPlpgsqlTypeMetadata,
) -> PgQueryCatalogLookupResult {
    let context = &*(context as *const CatalogContext<'_>);
    match context.catalog.types.iter().position(|ty| ty.oid == oid) {
        Some(index) => {
            context.write_type(index, output);
            PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_FOUND
        }
        None => PgQueryCatalogLookupResult_PG_QUERY_CATALOG_LOOKUP_NOT_FOUND,
    }
}

unsafe extern "C" fn catalog_error(_context: *mut c_void) -> *const std::os::raw::c_char {
    std::ptr::null()
}

/// Parse PL/pgSQL with the caller's catalog types instead of treating every unknown type as a record. The snapshot and callback storage remain local to this synchronous parse; no pointers or callback state escape it.
pub fn parse_plpgsql_with_catalog(
    stmt: &str,
    catalog: &PlpgsqlCatalog,
) -> Result<serde_json::Value> {
    let input = CString::new(stmt)?;
    let mut context = CatalogContext {
        catalog,
        names: catalog
            .types
            .iter()
            .map(|ty| CString::new(ty.name.as_str()))
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    let callbacks = PgQueryPlpgsqlCatalog {
        context: (&mut context as *mut CatalogContext<'_>).cast(),
        lookup_namespace: Some(lookup_namespace),
        lookup_type_by_name: Some(lookup_type_by_name),
        lookup_type_by_oid: Some(lookup_type_by_oid),
        get_error: Some(catalog_error),
    };
    // All callbacks only inspect the immutable snapshot. Its C strings and context outlive the C parser's synchronous callback scope.
    let result = unsafe { pg_query_parse_plpgsql_with_catalog(input.as_ptr(), &callbacks) };
    let structure = if !result.error.is_null() {
        let message = unsafe { CStr::from_ptr((*result.error).message) }
            .to_string_lossy()
            .to_string();
        Err(Error::Parse(message))
    } else if result.plpgsql_funcs.is_null() {
        Err(Error::InvalidPointer)
    } else {
        let raw = unsafe { CStr::from_ptr(result.plpgsql_funcs) };
        serde_json::from_str(&raw.to_string_lossy())
            .map_err(|error| Error::InvalidJson(error.to_string()))
    };
    unsafe { pg_query_free_plpgsql_parse_result(result) };
    structure
}
