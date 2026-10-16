//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in B-tree class identities and default selection for resolved SQL types.

use crate::ColumnType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinBTreeOperatorClass {
    pub oid: u32,
    pub name: &'static str,
    pub default: bool,
    pub family_oid: u32,
    pub input_type_oid: u32,
    pub comparison_oid: u32,
}

/// Built-in catalog metadata, not an assertion that an arbitrary class has an executable UQA comparator or exact-key implementation.
pub fn builtin_btree_operator_classes() -> &'static [BuiltinBTreeOperatorClass] {
    &CLASSES
}

pub fn builtin_btree_operator_class(oid: u32) -> Option<&'static BuiltinBTreeOperatorClass> {
    CLASSES.iter().find(|class| class.oid == oid)
}

/// Select `PostgreSQL`'s built-in default class for the resolved type. Alias and generic-class binding preserve their declared type separately. UQA vector/tensor ordering has no `PostgreSQL` class identity.
pub fn default_btree_operator_class(ty: &ColumnType) -> Option<&'static BuiltinBTreeOperatorClass> {
    let input_type_oid = match ty {
        ColumnType::Domain { base, .. } => return default_btree_operator_class(base),
        ColumnType::Named(_)
        | ColumnType::Xid
        | ColumnType::Void
        | ColumnType::RefCursor
        | ColumnType::Json
        | ColumnType::PgNodeTree
        | ColumnType::AclItem
        | ColumnType::Vector(_)
        | ColumnType::Tensor(_) => return None,
        ColumnType::Varchar(_) => 25,
        ColumnType::Array(_) | ColumnType::AnyArray | ColumnType::Int2Vector => 2277,
        ColumnType::Record | ColumnType::Composite(_) => 2249,
        ColumnType::Enum(_) => 3500,
        ColumnType::Range(_) | ColumnType::DeclaredRange(_) => 3831,
        ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_) => 4537,
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regcollation
        | ColumnType::Regrole
        | ColumnType::Regtype => 26,
        _ => u32::try_from(crate::catalog::type_metadata::pg_type_oid(ty)).ok()?,
    };
    CLASSES
        .iter()
        .find(|class| class.default && class.input_type_oid == input_type_oid)
}

const CLASSES: [BuiltinBTreeOperatorClass; 44] = [
    BuiltinBTreeOperatorClass {
        oid: 1978,
        name: "int4_ops",
        default: true,
        family_oid: 1976,
        input_type_oid: 23,
        comparison_oid: 351,
    },
    BuiltinBTreeOperatorClass {
        oid: 1979,
        name: "int2_ops",
        default: true,
        family_oid: 1976,
        input_type_oid: 21,
        comparison_oid: 350,
    },
    BuiltinBTreeOperatorClass {
        oid: 1981,
        name: "oid_ops",
        default: true,
        family_oid: 1989,
        input_type_oid: 26,
        comparison_oid: 356,
    },
    BuiltinBTreeOperatorClass {
        oid: 3122,
        name: "date_ops",
        default: true,
        family_oid: 434,
        input_type_oid: 1082,
        comparison_oid: 1092,
    },
    BuiltinBTreeOperatorClass {
        oid: 3123,
        name: "float8_ops",
        default: true,
        family_oid: 1970,
        input_type_oid: 701,
        comparison_oid: 355,
    },
    BuiltinBTreeOperatorClass {
        oid: 3124,
        name: "int8_ops",
        default: true,
        family_oid: 1976,
        input_type_oid: 20,
        comparison_oid: 842,
    },
    BuiltinBTreeOperatorClass {
        oid: 3125,
        name: "numeric_ops",
        default: true,
        family_oid: 1988,
        input_type_oid: 1700,
        comparison_oid: 1769,
    },
    BuiltinBTreeOperatorClass {
        oid: 3126,
        name: "text_ops",
        default: true,
        family_oid: 1994,
        input_type_oid: 25,
        comparison_oid: 360,
    },
    BuiltinBTreeOperatorClass {
        oid: 3127,
        name: "timestamptz_ops",
        default: true,
        family_oid: 434,
        input_type_oid: 1184,
        comparison_oid: 1314,
    },
    BuiltinBTreeOperatorClass {
        oid: 3128,
        name: "timestamp_ops",
        default: true,
        family_oid: 434,
        input_type_oid: 1114,
        comparison_oid: 2045,
    },
    BuiltinBTreeOperatorClass {
        oid: 4217,
        name: "text_pattern_ops",
        default: false,
        family_oid: 2095,
        input_type_oid: 25,
        comparison_oid: 2166,
    },
    BuiltinBTreeOperatorClass {
        oid: 4218,
        name: "varchar_pattern_ops",
        default: false,
        family_oid: 2095,
        input_type_oid: 25,
        comparison_oid: 2166,
    },
    BuiltinBTreeOperatorClass {
        oid: 4219,
        name: "bpchar_pattern_ops",
        default: false,
        family_oid: 2097,
        input_type_oid: 1042,
        comparison_oid: 2180,
    },
    BuiltinBTreeOperatorClass {
        oid: 10000,
        name: "array_ops",
        default: true,
        family_oid: 397,
        input_type_oid: 2277,
        comparison_oid: 382,
    },
    BuiltinBTreeOperatorClass {
        oid: 10002,
        name: "bit_ops",
        default: true,
        family_oid: 423,
        input_type_oid: 1560,
        comparison_oid: 1596,
    },
    BuiltinBTreeOperatorClass {
        oid: 10003,
        name: "bool_ops",
        default: true,
        family_oid: 424,
        input_type_oid: 16,
        comparison_oid: 1693,
    },
    BuiltinBTreeOperatorClass {
        oid: 10004,
        name: "bpchar_ops",
        default: true,
        family_oid: 426,
        input_type_oid: 1042,
        comparison_oid: 1078,
    },
    BuiltinBTreeOperatorClass {
        oid: 10006,
        name: "bytea_ops",
        default: true,
        family_oid: 428,
        input_type_oid: 17,
        comparison_oid: 1954,
    },
    BuiltinBTreeOperatorClass {
        oid: 10007,
        name: "char_ops",
        default: true,
        family_oid: 429,
        input_type_oid: 18,
        comparison_oid: 358,
    },
    BuiltinBTreeOperatorClass {
        oid: 10009,
        name: "cidr_ops",
        default: false,
        family_oid: 1974,
        input_type_oid: 869,
        comparison_oid: 926,
    },
    BuiltinBTreeOperatorClass {
        oid: 10012,
        name: "float4_ops",
        default: true,
        family_oid: 1970,
        input_type_oid: 700,
        comparison_oid: 354,
    },
    BuiltinBTreeOperatorClass {
        oid: 10015,
        name: "inet_ops",
        default: true,
        family_oid: 1974,
        input_type_oid: 869,
        comparison_oid: 926,
    },
    BuiltinBTreeOperatorClass {
        oid: 10022,
        name: "interval_ops",
        default: true,
        family_oid: 1982,
        input_type_oid: 1186,
        comparison_oid: 1315,
    },
    BuiltinBTreeOperatorClass {
        oid: 10024,
        name: "macaddr_ops",
        default: true,
        family_oid: 1984,
        input_type_oid: 829,
        comparison_oid: 836,
    },
    BuiltinBTreeOperatorClass {
        oid: 10026,
        name: "macaddr8_ops",
        default: true,
        family_oid: 3371,
        input_type_oid: 774,
        comparison_oid: 4119,
    },
    BuiltinBTreeOperatorClass {
        oid: 10028,
        name: "name_ops",
        default: true,
        family_oid: 1994,
        input_type_oid: 19,
        comparison_oid: 359,
    },
    BuiltinBTreeOperatorClass {
        oid: 10032,
        name: "oidvector_ops",
        default: true,
        family_oid: 1991,
        input_type_oid: 30,
        comparison_oid: 404,
    },
    BuiltinBTreeOperatorClass {
        oid: 10034,
        name: "record_ops",
        default: true,
        family_oid: 2994,
        input_type_oid: 2249,
        comparison_oid: 2987,
    },
    BuiltinBTreeOperatorClass {
        oid: 10036,
        name: "record_image_ops",
        default: false,
        family_oid: 3194,
        input_type_oid: 2249,
        comparison_oid: 3187,
    },
    BuiltinBTreeOperatorClass {
        oid: 10038,
        name: "time_ops",
        default: true,
        family_oid: 1996,
        input_type_oid: 1083,
        comparison_oid: 1107,
    },
    BuiltinBTreeOperatorClass {
        oid: 10041,
        name: "timetz_ops",
        default: true,
        family_oid: 2000,
        input_type_oid: 1266,
        comparison_oid: 1358,
    },
    BuiltinBTreeOperatorClass {
        oid: 10043,
        name: "varbit_ops",
        default: true,
        family_oid: 2002,
        input_type_oid: 1562,
        comparison_oid: 1672,
    },
    BuiltinBTreeOperatorClass {
        oid: 10044,
        name: "varchar_ops",
        default: false,
        family_oid: 1994,
        input_type_oid: 25,
        comparison_oid: 360,
    },
    BuiltinBTreeOperatorClass {
        oid: 10047,
        name: "money_ops",
        default: true,
        family_oid: 2099,
        input_type_oid: 790,
        comparison_oid: 377,
    },
    BuiltinBTreeOperatorClass {
        oid: 10050,
        name: "tid_ops",
        default: true,
        family_oid: 2789,
        input_type_oid: 27,
        comparison_oid: 2794,
    },
    BuiltinBTreeOperatorClass {
        oid: 10053,
        name: "xid8_ops",
        default: true,
        family_oid: 5067,
        input_type_oid: 5069,
        comparison_oid: 5096,
    },
    BuiltinBTreeOperatorClass {
        oid: 10065,
        name: "uuid_ops",
        default: true,
        family_oid: 2968,
        input_type_oid: 2950,
        comparison_oid: 2960,
    },
    BuiltinBTreeOperatorClass {
        oid: 10067,
        name: "pg_lsn_ops",
        default: true,
        family_oid: 3253,
        input_type_oid: 3220,
        comparison_oid: 3251,
    },
    BuiltinBTreeOperatorClass {
        oid: 10069,
        name: "enum_ops",
        default: true,
        family_oid: 3522,
        input_type_oid: 3500,
        comparison_oid: 3514,
    },
    BuiltinBTreeOperatorClass {
        oid: 10071,
        name: "tsvector_ops",
        default: true,
        family_oid: 3626,
        input_type_oid: 3614,
        comparison_oid: 3622,
    },
    BuiltinBTreeOperatorClass {
        oid: 10074,
        name: "tsquery_ops",
        default: true,
        family_oid: 3683,
        input_type_oid: 3615,
        comparison_oid: 3668,
    },
    BuiltinBTreeOperatorClass {
        oid: 10076,
        name: "range_ops",
        default: true,
        family_oid: 3901,
        input_type_oid: 3831,
        comparison_oid: 3870,
    },
    BuiltinBTreeOperatorClass {
        oid: 10080,
        name: "multirange_ops",
        default: true,
        family_oid: 4199,
        input_type_oid: 4537,
        comparison_oid: 4273,
    },
    BuiltinBTreeOperatorClass {
        oid: 10088,
        name: "jsonb_ops",
        default: true,
        family_oid: 4033,
        input_type_oid: 3802,
        comparison_oid: 4044,
    },
];

#[cfg(test)]
mod tests;
