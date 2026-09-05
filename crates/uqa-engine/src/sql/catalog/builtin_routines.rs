//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in routine metadata exposed through the virtual catalogs.

#[derive(Debug, Clone, Copy)]
pub(super) struct BuiltinRoutineCatalogEntry {
    pub(super) oid: i64,
    pub(super) name: &'static str,
    pub(super) kind: &'static str,
    pub(super) strict: bool,
    pub(super) volatility: &'static str,
    pub(super) parallel: &'static str,
    pub(super) leakproof: bool,
    pub(super) return_type: i64,
    pub(super) argument_types: &'static [i64],
    pub(super) argument_names: &'static [&'static str],
    pub(super) default_arguments: usize,
    pub(super) argument_defaults: Option<&'static str>,
    pub(super) source: &'static str,
}

impl BuiltinRoutineCatalogEntry {
    pub(super) const fn language(self) -> i64 {
        match self.oid {
            1810 | 1811 => 14,
            _ => 12,
        }
    }

    pub(super) fn sql_body(self) -> Option<String> {
        let (function_oid, parameter_type, collation_oid) = match self.oid {
            1810 => (720, 17, 0),
            1811 => (1374, 25, 100),
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

    pub(super) const fn variadic_type(self) -> i64 {
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

    pub(super) const fn all_argument_types(self) -> Option<&'static [i64]> {
        match self.oid {
            3078 => Some(&[26, 20, 20, 20, 20, 16, 20, 26]),
            6427 => Some(&[2205, 20, 16]),
            _ => None,
        }
    }

    pub(super) const fn argument_modes(self) -> Option<&'static [&'static str]> {
        match self.oid {
            3078 => Some(&["i", "o", "o", "o", "o", "o", "o", "o"]),
            6427 => Some(&["i", "o", "o"]),
            _ => None,
        }
    }

    pub(super) const fn returns_set(self) -> bool {
        self.oid == 3035
    }

    pub(super) const fn estimated_rows(self) -> f64 {
        if self.oid == 3035 {
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

mod definitions;
mod notifications;
mod privileges;
mod ranges;
mod scalar;
mod sequences;

pub(super) const PG18_BUILTIN_ROUTINE_GROUPS: &[&[BuiltinRoutineCatalogEntry]] = &[
    scalar::ROUTINES,
    definitions::ROUTINES,
    notifications::ROUTINES,
    privileges::ROUTINES,
    ranges::ROUTINES,
    sequences::ROUTINES,
];
