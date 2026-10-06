//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 temporal extraction, truncation and interval normalization identities.

use super::BuiltinRoutineCatalogEntry;

const fn routine(
    oid: i64,
    name: &'static str,
    volatility: &'static str,
    return_type: i64,
    argument_types: &'static [i64],
    source: &'static str,
) -> BuiltinRoutineCatalogEntry {
    BuiltinRoutineCatalogEntry {
        oid,
        name,
        kind: "f",
        strict: true,
        volatility,
        parallel: "s",
        leakproof: false,
        return_type,
        argument_types,
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source,
    }
}

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    routine(1171, "date_part", "s", 701, &[25, 1184], "timestamptz_part"),
    routine(1172, "date_part", "i", 701, &[25, 1186], "interval_part"),
    routine(1273, "date_part", "i", 701, &[25, 1266], "timetz_part"),
    routine(1384, "date_part", "i", 701, &[25, 1082], ""),
    routine(1385, "date_part", "i", 701, &[25, 1083], "time_part"),
    routine(2021, "date_part", "i", 701, &[25, 1114], "timestamp_part"),
    routine(6199, "extract", "i", 1700, &[25, 1082], "extract_date"),
    routine(6200, "extract", "i", 1700, &[25, 1083], "extract_time"),
    routine(6201, "extract", "i", 1700, &[25, 1266], "extract_timetz"),
    routine(6202, "extract", "i", 1700, &[25, 1114], "extract_timestamp"),
    routine(
        6203,
        "extract",
        "s",
        1700,
        &[25, 1184],
        "extract_timestamptz",
    ),
    routine(6204, "extract", "i", 1700, &[25, 1186], "extract_interval"),
    routine(
        1175,
        "justify_hours",
        "i",
        1186,
        &[1186],
        "interval_justify_hours",
    ),
    routine(
        1217,
        "date_trunc",
        "s",
        1184,
        &[25, 1184],
        "timestamptz_trunc",
    ),
    routine(1218, "date_trunc", "i", 1186, &[25, 1186], "interval_trunc"),
    routine(
        1284,
        "date_trunc",
        "i",
        1184,
        &[25, 1184, 25],
        "timestamptz_trunc_zone",
    ),
    routine(
        1295,
        "justify_days",
        "i",
        1186,
        &[1186],
        "interval_justify_days",
    ),
    routine(
        2020,
        "date_trunc",
        "i",
        1114,
        &[25, 1114],
        "timestamp_trunc",
    ),
    routine(
        2711,
        "justify_interval",
        "i",
        1186,
        &[1186],
        "interval_justify_interval",
    ),
];

// PostgreSQL 18 pg_proc.dat SQL-standard body for date_part(text, date).
pub(super) const DATE_PART_DATE_SQL_BODY: &str = concat!(
    "{QUERY :commandType 1 :querySource 0 :canSetTag true :utilityStmt <> :resultRelation 0 ",
    ":hasAggs false :hasWindowFuncs false :hasTargetSRFs false :hasSubLinks false ",
    ":hasDistinctOn false :hasRecursive false :hasModifyingCTE false :hasForUpdate false ",
    ":hasRowSecurity false :hasGroupRTE false :isReturn true :cteList <> :rtable <> ",
    ":rteperminfos <> :jointree {FROMEXPR :fromlist <> :quals <>} :mergeActionList <> ",
    ":mergeTargetRelation 0 :mergeJoinCondition <> :targetList ({TARGETENTRY :expr {FUNCEXPR ",
    ":funcid 2021 :funcresulttype 701 :funcretset false :funcvariadic false :funcformat 0 ",
    ":funccollid 0 :inputcollid 100 :args ({PARAM :paramkind 0 :paramid 1 :paramtype 25 ",
    ":paramtypmod -1 :paramcollid 100 :location -1} {FUNCEXPR :funcid 2024 :funcresulttype ",
    "1114 :funcretset false :funcvariadic false :funcformat 1 :funccollid 0 :inputcollid 0 ",
    ":args ({PARAM :paramkind 0 :paramid 2 :paramtype 1082 :paramtypmod -1 :paramcollid 0 ",
    ":location -1}) :location -1}) :location -1} :resno 1 :resname <> :ressortgroupref 0 ",
    ":resorigtbl 0 :resorigcol 0 :resjunk false}) :override 0 :onConflict <> ",
    ":returningOldAlias <> :returningNewAlias <> :returningList <> :groupClause <> ",
    ":groupDistinct false :groupingSets <> :havingQual <> :windowClause <> :distinctClause <> ",
    ":sortClause <> :limitOffset <> :limitCount <> :limitOption 0 :rowMarks <> :setOperations ",
    "<> :constraintDeps <> :withCheckOptions <> :stmt_location -1 :stmt_len -1}",
);
