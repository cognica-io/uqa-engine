//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` identities shared by JSON path overload selection and catalog projection.

use crate::{ast::FunctionDispatch, ColumnType};

#[derive(Debug, Clone, Copy)]
pub struct JsonPathFunction {
    pub name: &'static str,
    pub oid: i64,
    pub argument_oids: &'static [i64],
    pub result_oid: i64,
    pub jsonb: bool,
    pub as_text: bool,
}

pub const JSON_PATH_FUNCTIONS: [JsonPathFunction; 4] = [
    JsonPathFunction {
        name: "json_extract_path",
        oid: 3951,
        argument_oids: &[114, 1009],
        result_oid: 114,
        jsonb: false,
        as_text: false,
    },
    JsonPathFunction {
        name: "json_extract_path_text",
        oid: 3953,
        argument_oids: &[114, 1009],
        result_oid: 25,
        jsonb: false,
        as_text: true,
    },
    JsonPathFunction {
        name: "jsonb_extract_path",
        oid: 3217,
        argument_oids: &[3802, 1009],
        result_oid: 3802,
        jsonb: true,
        as_text: false,
    },
    JsonPathFunction {
        name: "jsonb_extract_path_text",
        oid: 3940,
        argument_oids: &[3802, 1009],
        result_oid: 25,
        jsonb: true,
        as_text: true,
    },
];

impl JsonPathFunction {
    #[must_use]
    pub const fn by_oid(oid: i64) -> Option<Self> {
        let mut index = 0;
        while index < JSON_PATH_FUNCTIONS.len() {
            if JSON_PATH_FUNCTIONS[index].oid == oid {
                return Some(JSON_PATH_FUNCTIONS[index]);
            }
            index += 1;
        }
        None
    }

    #[must_use]
    pub fn lookup(name: &str) -> Option<Self> {
        JSON_PATH_FUNCTIONS.into_iter().find(|entry| {
            name.eq_ignore_ascii_case(entry.name)
                || name
                    .get(..11)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
                    && name[11..].eq_ignore_ascii_case(entry.name)
        })
    }

    #[must_use]
    pub const fn input_type(self) -> ColumnType {
        if self.jsonb {
            ColumnType::JsonB
        } else {
            ColumnType::Json
        }
    }

    #[must_use]
    pub const fn result_type(self) -> ColumnType {
        if self.as_text {
            ColumnType::Text
        } else {
            self.input_type()
        }
    }

    #[must_use]
    pub const fn dispatch(self) -> FunctionDispatch {
        FunctionDispatch::JsonPath {
            jsonb: self.jsonb,
            as_text: self.as_text,
        }
    }
}
