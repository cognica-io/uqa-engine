//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in routine metadata exposed through the virtual catalogs.

mod identities;
pub use identities::{
    builtin_routine_identities, builtin_routine_identity, BuiltinRoutineIdentity,
};

use uqa_sql::catalog::languages::{INTERNAL_LANGUAGE, SQL_LANGUAGE};

#[derive(Debug, Clone, Copy)]
pub struct BuiltinRoutineCatalogEntry {
    pub oid: i64,
    pub name: &'static str,
    pub kind: &'static str,
    pub strict: bool,
    pub volatility: &'static str,
    pub parallel: &'static str,
    pub leakproof: bool,
    pub return_type: i64,
    pub argument_types: &'static [i64],
    pub argument_names: &'static [&'static str],
    pub default_arguments: usize,
    pub argument_defaults: Option<&'static str>,
    pub source: &'static str,
}

impl BuiltinRoutineCatalogEntry {
    pub const fn language(self) -> i64 {
        match self.oid {
            1384 | 1810 | 1811 | 3935 | 3936 => SQL_LANGUAGE as i64,
            _ => INTERNAL_LANGUAGE as i64,
        }
    }

    pub fn sql_body(self) -> Option<String> {
        let (function_oid, parameter_type, collation_oid) = match self.oid {
            1384 => return Some(temporal::DATE_PART_DATE_SQL_BODY.into()),
            1810 => (720, 17, 0),
            1811 => (1374, 25, 100),
            3935 => return Some(sleep_bodies::PG_SLEEP_FOR_SQL_BODY.into()),
            3936 => return Some(sleep_bodies::PG_SLEEP_UNTIL_SQL_BODY.into()),
            _ => return None,
        };
        let mut body = String::from(BIT_LENGTH_SQL_BODY_PREFIX);
        body.push_str(&function_oid.to_string());
        body.push_str(BIT_LENGTH_SQL_BODY_AFTER_FUNCTION);
        body.push_str(&collation_oid.to_string());
        body.push_str(BIT_LENGTH_SQL_BODY_AFTER_INPUT_COLLATION);
        body.push_str(&parameter_type.to_string());
        body.push_str(BIT_LENGTH_SQL_BODY_AFTER_PARAMETER_TYPE);
        body.push_str(&collation_oid.to_string());
        body.push_str(BIT_LENGTH_SQL_BODY_SUFFIX);
        Some(body)
    }

    pub const fn variadic_type(self) -> i64 {
        match self.oid {
            4282 => 3904,
            4285 => 3906,
            4288 => 3908,
            4291 => 3910,
            4294 => 3912,
            4297 => 3926,
            _ => 0,
        }
    }

    pub const fn all_argument_types(self) -> Option<&'static [i64]> {
        match self.oid {
            3078 => Some(&[26, 20, 20, 20, 20, 16, 20, 26]),
            6427 => Some(&[2205, 20, 16]),
            1689 => Some(&[1034, 26, 26, 25, 16]),
            _ => None,
        }
    }

    pub const fn argument_modes(self) -> Option<&'static [&'static str]> {
        match self.oid {
            3078 => Some(&["i", "o", "o", "o", "o", "o", "o", "o"]),
            6427 => Some(&["i", "o", "o"]),
            1689 => Some(&["i", "o", "o", "o", "o"]),
            _ => None,
        }
    }

    pub const fn returns_set(self) -> bool {
        matches!(self.oid, 1066..=1069 | 3035 | 1689)
    }

    pub const fn support_oid(self) -> i64 {
        match self.oid {
            1066 | 1067 => 3994,
            1068 | 1069 => 3995,
            3100 => 6233,
            3101 => 6234,
            3102 => 6235,
            3103 => 6306,
            3104 => 6307,
            3105 => 6308,
            6179 => 6380,
            _ => 0,
        }
    }

    pub const fn estimated_rows(self) -> f64 {
        if matches!(self.oid, 1066..=1069) {
            1000.0
        } else if self.returns_set() {
            10.0
        } else {
            0.0
        }
    }
}

macro_rules! range_routine {
    ($oid:literal, $name:literal, $strict:literal, $return_type:literal, [$($argument_type:literal),*], $source:literal) => {
        BuiltinRoutineCatalogEntry {
            oid: $oid,
            name: $name,
            kind: "f",
            strict: $strict,
            volatility: "i",
            parallel: "s",
            leakproof: false,
            return_type: $return_type,
            argument_types: &[$($argument_type),*],
            argument_names: &[],
            default_arguments: 0,
            argument_defaults: None,
            source: $source,
        }
    };
}

const BIT_LENGTH_SQL_BODY_PREFIX: &str = concat!(
    "{QUERY :commandType 1 :querySource 0 :canSetTag true :utilityStmt <> ",
    ":resultRelation 0 :hasAggs false :hasWindowFuncs false :hasTargetSRFs false ",
    ":hasSubLinks false :hasDistinctOn false :hasRecursive false :hasModifyingCTE false ",
    ":hasForUpdate false :hasRowSecurity false :hasGroupRTE false :isReturn true :cteList <> ",
    ":rtable <> :rteperminfos <> :jointree {FROMEXPR :fromlist <> :quals <>} ",
    ":mergeActionList <> :mergeTargetRelation 0 :mergeJoinCondition <> ",
    ":targetList ({TARGETENTRY :expr {OPEXPR :opno 514 :opfuncid 141 :opresulttype 23 ",
    ":opretset false :opcollid 0 :inputcollid 0 :args ({FUNCEXPR :funcid "
);
const BIT_LENGTH_SQL_BODY_AFTER_FUNCTION: &str = concat!(
    " :funcresulttype 23 :funcretset false :funcvariadic false :funcformat 0 ",
    ":funccollid 0 :inputcollid "
);
const BIT_LENGTH_SQL_BODY_AFTER_INPUT_COLLATION: &str =
    " :args ({PARAM :paramkind 0 :paramid 1 :paramtype ";
const BIT_LENGTH_SQL_BODY_AFTER_PARAMETER_TYPE: &str = " :paramtypmod -1 :paramcollid ";
const BIT_LENGTH_SQL_BODY_SUFFIX: &str = concat!(
    " :location -1}) :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 ",
    ":constlen 4 :constbyval true :constisnull false :location -1 ",
    ":constvalue 4 [ 8 0 0 0 0 0 0 0 ]}) :location -1} :resno 1 :resname <> ",
    ":ressortgroupref 0 :resorigtbl 0 :resorigcol 0 :resjunk false}) :override 0 ",
    ":onConflict <> :returningOldAlias <> :returningNewAlias <> :returningList <> ",
    ":groupClause <> :groupDistinct false :groupingSets <> :havingQual <> :windowClause <> ",
    ":distinctClause <> :sortClause <> :limitOffset <> :limitCount <> :limitOption 0 ",
    ":rowMarks <> :setOperations <> :constraintDeps <> :withCheckOptions <> ",
    ":stmt_location -1 :stmt_len -1}"
);

const FALSE_NODE: &str = "({CONST :consttype 16 :consttypmod -1 :constcollid 0 :constlen 1 :constbyval true :constisnull false :location -1 :constvalue 1 [ 0 0 0 0 0 0 0 0 ]})";

mod aggregate_windows;
mod arrays;
mod clock_and_case;
mod definitions;
mod enums;
mod namespaces;
mod notifications;
mod numeric;
mod privileges;
mod ranges;
mod records;
mod scalar;
mod sequences;
mod series;
mod sleep_bodies;
mod support;
mod temporal;

const fn subscript_handler(oid: i64, name: &'static str) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility: "i",
        parallel: "s",
        leakproof: false,
        return_type: 2281,
        argument_types: &[2281],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: name,
    }
}

pub const PG18_BUILTIN_ROUTINE_GROUPS: &[&[BuiltinRoutineCatalogEntry]] = &[
    support::ROUTINES,
    scalar::ROUTINES,
    aggregate_windows::ROUTINES,
    clock_and_case::ROUTINES,
    numeric::ROUTINES,
    temporal::ROUTINES,
    arrays::ROUTINES,
    enums::ROUTINES,
    definitions::ROUTINES,
    namespaces::ROUTINES,
    records::ROUTINES,
    notifications::ROUTINES,
    privileges::ROUTINES,
    ranges::ROUTINES,
    sequences::ROUTINES,
    series::ROUTINES,
    &[
        subscript_handler(6098, "jsonb_subscript_handler"),
        subscript_handler(6179, "array_subscript_handler"),
        subscript_handler(6180, "raw_array_subscript_handler"),
    ],
];

#[cfg(test)]
mod tests;
