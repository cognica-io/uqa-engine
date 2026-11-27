//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 `pg_cast` source, target, coercion method, context, and function identities.

/// Public identity and coercion properties of a row in `pg_cast`.
#[derive(Clone, Copy, Debug)]
pub struct CastCatalogRow {
    pub oid: u32,
    pub source: u32,
    pub target: u32,
    pub function: u32,
    pub context: u8,
    pub method: u8,
}

pub fn builtin_casts() -> impl Iterator<Item = CastCatalogRow> {
    CASTS.iter().map(
        |&(_, (oid, source, target), (context, method, function, _))| CastCatalogRow {
            oid,
            source,
            target,
            function,
            context,
            method,
        },
    )
}

pub(super) fn entry(source: &str, target: &str) -> Option<(u8, u8, i64, usize)> {
    let index = CASTS
        .binary_search_by_key(&(source, target), |&(names, ..)| names)
        .ok()?;
    let (context, method, function, arity) = CASTS[index].2;
    Some((context, method, i64::from(function), arity))
}

pub(super) fn context(source: &str, target: &str) -> Option<u8> {
    entry(source, target).map(|entry| entry.0)
}

// Type names, catalog addresses (cast, source, target), and coercion properties in lookup order.
type CastDefinition = (
    (&'static str, &'static str),
    (u32, u32, u32),
    (u8, u8, u32, usize),
);
const CASTS: &[CastDefinition] = &[
    (("bit", "bit"), (10217, 1560, 1560), (b'i', b'f', 1685, 3)),
    (("bit", "int4"), (10194, 1560, 23), (b'e', b'f', 1684, 1)),
    (("bit", "int8"), (10193, 1560, 20), (b'e', b'f', 2076, 1)),
    (("bit", "varbit"), (10189, 1560, 1562), (b'i', b'b', 0, 0)),
    (("bool", "bpchar"), (10207, 16, 1042), (b'a', b'f', 2971, 1)),
    (("bool", "int4"), (10035, 16, 23), (b'e', b'f', 2558, 1)),
    (("bool", "text"), (10197, 16, 25), (b'a', b'f', 2971, 1)),
    (
        ("bool", "varchar"),
        (10202, 16, 1043),
        (b'a', b'f', 2971, 1),
    ),
    (("box", "circle"), (10177, 603, 718), (b'e', b'f', 1479, 1)),
    (("box", "lseg"), (10175, 603, 601), (b'e', b'f', 1541, 1)),
    (("box", "point"), (10174, 603, 600), (b'e', b'f', 1534, 1)),
    (("box", "polygon"), (10176, 603, 604), (b'a', b'f', 1448, 1)),
    (
        ("bpchar", "bpchar"),
        (10210, 1042, 1042),
        (b'i', b'f', 668, 3),
    ),
    (("bpchar", "char"), (10138, 1042, 18), (b'a', b'f', 944, 1)),
    (("bpchar", "name"), (10141, 1042, 19), (b'i', b'f', 409, 1)),
    (("bpchar", "text"), (10127, 1042, 25), (b'i', b'f', 401, 1)),
    (
        ("bpchar", "varchar"),
        (10128, 1042, 1043),
        (b'i', b'f', 401, 1),
    ),
    (("bpchar", "xml"), (10209, 1042, 142), (b'e', b'f', 2896, 1)),
    (("bytea", "int2"), (10146, 17, 21), (b'e', b'f', 6370, 1)),
    (("bytea", "int4"), (10147, 17, 23), (b'e', b'f', 6371, 1)),
    (("bytea", "int8"), (10148, 17, 20), (b'e', b'f', 6372, 1)),
    (("char", "bpchar"), (10132, 18, 1042), (b'a', b'f', 860, 1)),
    (("char", "int4"), (10149, 18, 23), (b'e', b'f', 77, 1)),
    (("char", "text"), (10131, 18, 25), (b'i', b'f', 946, 1)),
    (("char", "varchar"), (10133, 18, 1043), (b'a', b'f', 946, 1)),
    (("cidr", "bpchar"), (10205, 650, 1042), (b'a', b'f', 730, 1)),
    (("cidr", "inet"), (10187, 650, 869), (b'i', b'b', 0, 0)),
    (("cidr", "text"), (10195, 650, 25), (b'a', b'f', 730, 1)),
    (
        ("cidr", "varchar"),
        (10200, 650, 1043),
        (b'a', b'f', 730, 1),
    ),
    (("circle", "box"), (10183, 718, 603), (b'e', b'f', 1480, 1)),
    (
        ("circle", "point"),
        (10182, 718, 600),
        (b'e', b'f', 1416, 1),
    ),
    (
        ("circle", "polygon"),
        (10184, 718, 604),
        (b'e', b'f', 1544, 1),
    ),
    (
        ("date", "timestamp"),
        (10158, 1082, 1114),
        (b'i', b'f', 2024, 1),
    ),
    (
        ("date", "timestamptz"),
        (10159, 1082, 1184),
        (b'i', b'f', 1174, 1),
    ),
    (
        ("daterange", "datemultirange"),
        (10232, 3912, 4535),
        (b'e', b'f', 4293, 1),
    ),
    (
        ("float4", "float8"),
        (10018, 700, 701),
        (b'i', b'f', 311, 1),
    ),
    (("float4", "int2"), (10016, 700, 21), (b'a', b'f', 238, 1)),
    (("float4", "int4"), (10017, 700, 23), (b'a', b'f', 319, 1)),
    (("float4", "int8"), (10015, 700, 20), (b'a', b'f', 653, 1)),
    (
        ("float4", "numeric"),
        (10019, 700, 1700),
        (b'a', b'f', 1742, 1),
    ),
    (
        ("float8", "float4"),
        (10023, 701, 700),
        (b'a', b'f', 312, 1),
    ),
    (("float8", "int2"), (10021, 701, 21), (b'a', b'f', 237, 1)),
    (("float8", "int4"), (10022, 701, 23), (b'a', b'f', 317, 1)),
    (("float8", "int8"), (10020, 701, 20), (b'a', b'f', 483, 1)),
    (
        ("float8", "numeric"),
        (10024, 701, 1700),
        (b'a', b'f', 1743, 1),
    ),
    (("inet", "bpchar"), (10206, 869, 1042), (b'a', b'f', 730, 1)),
    (("inet", "cidr"), (10188, 869, 650), (b'a', b'f', 1715, 1)),
    (("inet", "text"), (10196, 869, 25), (b'a', b'f', 730, 1)),
    (
        ("inet", "varchar"),
        (10201, 869, 1043),
        (b'a', b'f', 730, 1),
    ),
    (("int2", "bytea"), (10143, 21, 17), (b'e', b'f', 6367, 1)),
    (("int2", "float4"), (10007, 21, 700), (b'i', b'f', 236, 1)),
    (("int2", "float8"), (10008, 21, 701), (b'i', b'f', 235, 1)),
    (("int2", "int4"), (10006, 21, 23), (b'i', b'f', 313, 1)),
    (("int2", "int8"), (10005, 21, 20), (b'i', b'f', 754, 1)),
    (
        ("int2", "numeric"),
        (10009, 21, 1700),
        (b'i', b'f', 1782, 1),
    ),
    (("int2", "oid"), (10038, 21, 26), (b'i', b'f', 313, 1)),
    (
        ("int2", "regclass"),
        (10077, 21, 2205),
        (b'i', b'f', 313, 1),
    ),
    (
        ("int2", "regcollation"),
        (10084, 21, 4191),
        (b'i', b'f', 313, 1),
    ),
    (
        ("int2", "regconfig"),
        (10098, 21, 3734),
        (b'i', b'f', 313, 1),
    ),
    (
        ("int2", "regdictionary"),
        (10105, 21, 3769),
        (b'i', b'f', 313, 1),
    ),
    (
        ("int2", "regnamespace"),
        (10121, 21, 4089),
        (b'i', b'f', 313, 1),
    ),
    (("int2", "regoper"), (10061, 21, 2203), (b'i', b'f', 313, 1)),
    (
        ("int2", "regoperator"),
        (10070, 21, 2204),
        (b'i', b'f', 313, 1),
    ),
    (("int2", "regproc"), (10045, 21, 24), (b'i', b'f', 313, 1)),
    (
        ("int2", "regprocedure"),
        (10054, 21, 2202),
        (b'i', b'f', 313, 1),
    ),
    (("int2", "regrole"), (10114, 21, 4096), (b'i', b'f', 313, 1)),
    (("int2", "regtype"), (10091, 21, 2206), (b'i', b'f', 313, 1)),
    (("int4", "bit"), (10192, 23, 1560), (b'e', b'f', 1683, 2)),
    (("int4", "bool"), (10034, 23, 16), (b'e', b'f', 2557, 1)),
    (("int4", "bytea"), (10144, 23, 17), (b'e', b'f', 6368, 1)),
    (("int4", "char"), (10150, 23, 18), (b'e', b'f', 78, 1)),
    (("int4", "float4"), (10012, 23, 700), (b'i', b'f', 318, 1)),
    (("int4", "float8"), (10013, 23, 701), (b'i', b'f', 316, 1)),
    (("int4", "int2"), (10011, 23, 21), (b'a', b'f', 314, 1)),
    (("int4", "int8"), (10010, 23, 20), (b'i', b'f', 481, 1)),
    (("int4", "money"), (10032, 23, 790), (b'a', b'f', 3811, 1)),
    (
        ("int4", "numeric"),
        (10014, 23, 1700),
        (b'i', b'f', 1740, 1),
    ),
    (("int4", "oid"), (10039, 23, 26), (b'i', b'b', 0, 0)),
    (("int4", "regclass"), (10078, 23, 2205), (b'i', b'b', 0, 0)),
    (
        ("int4", "regcollation"),
        (10085, 23, 4191),
        (b'i', b'b', 0, 0),
    ),
    (("int4", "regconfig"), (10099, 23, 3734), (b'i', b'b', 0, 0)),
    (
        ("int4", "regdictionary"),
        (10106, 23, 3769),
        (b'i', b'b', 0, 0),
    ),
    (
        ("int4", "regnamespace"),
        (10122, 23, 4089),
        (b'i', b'b', 0, 0),
    ),
    (("int4", "regoper"), (10062, 23, 2203), (b'i', b'b', 0, 0)),
    (
        ("int4", "regoperator"),
        (10071, 23, 2204),
        (b'i', b'b', 0, 0),
    ),
    (("int4", "regproc"), (10046, 23, 24), (b'i', b'b', 0, 0)),
    (
        ("int4", "regprocedure"),
        (10055, 23, 2202),
        (b'i', b'b', 0, 0),
    ),
    (("int4", "regrole"), (10115, 23, 4096), (b'i', b'b', 0, 0)),
    (("int4", "regtype"), (10092, 23, 2206), (b'i', b'b', 0, 0)),
    (
        ("int4range", "int4multirange"),
        (10229, 3904, 4451),
        (b'e', b'f', 4281, 1),
    ),
    (("int8", "bit"), (10191, 20, 1560), (b'e', b'f', 2075, 2)),
    (("int8", "bytea"), (10145, 20, 17), (b'e', b'f', 6369, 1)),
    (("int8", "float4"), (10002, 20, 700), (b'i', b'f', 652, 1)),
    (("int8", "float8"), (10003, 20, 701), (b'i', b'f', 482, 1)),
    (("int8", "int2"), (10000, 20, 21), (b'a', b'f', 714, 1)),
    (("int8", "int4"), (10001, 20, 23), (b'a', b'f', 480, 1)),
    (("int8", "money"), (10033, 20, 790), (b'a', b'f', 3812, 1)),
    (
        ("int8", "numeric"),
        (10004, 20, 1700),
        (b'i', b'f', 1781, 1),
    ),
    (("int8", "oid"), (10037, 20, 26), (b'i', b'f', 1287, 1)),
    (
        ("int8", "regclass"),
        (10076, 20, 2205),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regcollation"),
        (10083, 20, 4191),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regconfig"),
        (10097, 20, 3734),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regdictionary"),
        (10104, 20, 3769),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regnamespace"),
        (10120, 20, 4089),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regoper"),
        (10060, 20, 2203),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regoperator"),
        (10069, 20, 2204),
        (b'i', b'f', 1287, 1),
    ),
    (("int8", "regproc"), (10044, 20, 24), (b'i', b'f', 1287, 1)),
    (
        ("int8", "regprocedure"),
        (10053, 20, 2202),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regrole"),
        (10113, 20, 4096),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8", "regtype"),
        (10090, 20, 2206),
        (b'i', b'f', 1287, 1),
    ),
    (
        ("int8range", "int8multirange"),
        (10230, 3926, 4536),
        (b'e', b'f', 4296, 1),
    ),
    (
        ("interval", "interval"),
        (10215, 1186, 1186),
        (b'i', b'f', 1200, 2),
    ),
    (
        ("interval", "time"),
        (10169, 1186, 1083),
        (b'a', b'f', 1419, 1),
    ),
    (("json", "jsonb"), (10220, 114, 3802), (b'a', b'i', 0, 0)),
    (("jsonb", "bool"), (10222, 3802, 16), (b'e', b'f', 3556, 1)),
    (
        ("jsonb", "float4"),
        (10227, 3802, 700),
        (b'e', b'f', 3453, 1),
    ),
    (
        ("jsonb", "float8"),
        (10228, 3802, 701),
        (b'e', b'f', 2580, 1),
    ),
    (("jsonb", "int2"), (10224, 3802, 21), (b'e', b'f', 3450, 1)),
    (("jsonb", "int4"), (10225, 3802, 23), (b'e', b'f', 3451, 1)),
    (("jsonb", "int8"), (10226, 3802, 20), (b'e', b'f', 3452, 1)),
    (("jsonb", "json"), (10221, 3802, 114), (b'a', b'i', 0, 0)),
    (
        ("jsonb", "numeric"),
        (10223, 3802, 1700),
        (b'e', b'f', 3449, 1),
    ),
    (("lseg", "point"), (10172, 601, 600), (b'e', b'f', 1532, 1)),
    (
        ("macaddr", "macaddr8"),
        (10185, 829, 774),
        (b'i', b'f', 4123, 1),
    ),
    (
        ("macaddr8", "macaddr"),
        (10186, 774, 829),
        (b'i', b'f', 4124, 1),
    ),
    (
        ("money", "numeric"),
        (10030, 790, 1700),
        (b'a', b'f', 3823, 1),
    ),
    (("name", "bpchar"), (10135, 19, 1042), (b'a', b'f', 408, 1)),
    (("name", "text"), (10134, 19, 25), (b'i', b'f', 406, 1)),
    (
        ("name", "varchar"),
        (10136, 19, 1043),
        (b'a', b'f', 1401, 1),
    ),
    (
        ("numeric", "float4"),
        (10028, 1700, 700),
        (b'i', b'f', 1745, 1),
    ),
    (
        ("numeric", "float8"),
        (10029, 1700, 701),
        (b'i', b'f', 1746, 1),
    ),
    (
        ("numeric", "int2"),
        (10026, 1700, 21),
        (b'a', b'f', 1783, 1),
    ),
    (
        ("numeric", "int4"),
        (10027, 1700, 23),
        (b'a', b'f', 1744, 1),
    ),
    (
        ("numeric", "int8"),
        (10025, 1700, 20),
        (b'a', b'f', 1779, 1),
    ),
    (
        ("numeric", "money"),
        (10031, 1700, 790),
        (b'a', b'f', 3824, 1),
    ),
    (
        ("numeric", "numeric"),
        (10219, 1700, 1700),
        (b'i', b'f', 1703, 2),
    ),
    (
        ("numrange", "nummultirange"),
        (10231, 3906, 4532),
        (b'e', b'f', 4284, 1),
    ),
    (("oid", "int4"), (10041, 26, 23), (b'a', b'b', 0, 0)),
    (("oid", "int8"), (10040, 26, 20), (b'a', b'f', 1288, 1)),
    (("oid", "regclass"), (10074, 26, 2205), (b'i', b'b', 0, 0)),
    (
        ("oid", "regcollation"),
        (10081, 26, 4191),
        (b'i', b'b', 0, 0),
    ),
    (("oid", "regconfig"), (10095, 26, 3734), (b'i', b'b', 0, 0)),
    (
        ("oid", "regdictionary"),
        (10102, 26, 3769),
        (b'i', b'b', 0, 0),
    ),
    (
        ("oid", "regnamespace"),
        (10118, 26, 4089),
        (b'i', b'b', 0, 0),
    ),
    (("oid", "regoper"), (10058, 26, 2203), (b'i', b'b', 0, 0)),
    (
        ("oid", "regoperator"),
        (10067, 26, 2204),
        (b'i', b'b', 0, 0),
    ),
    (("oid", "regproc"), (10042, 26, 24), (b'i', b'b', 0, 0)),
    (
        ("oid", "regprocedure"),
        (10051, 26, 2202),
        (b'i', b'b', 0, 0),
    ),
    (("oid", "regrole"), (10111, 26, 4096), (b'i', b'b', 0, 0)),
    (("oid", "regtype"), (10088, 26, 2206), (b'i', b'b', 0, 0)),
    (
        ("path", "polygon"),
        (10173, 602, 604),
        (b'a', b'f', 1449, 1),
    ),
    (
        ("pg_dependencies", "bytea"),
        (10154, 3402, 17),
        (b'i', b'b', 0, 0),
    ),
    (
        ("pg_dependencies", "text"),
        (10155, 3402, 25),
        (b'i', b'i', 0, 0),
    ),
    (
        ("pg_mcv_list", "bytea"),
        (10156, 5017, 17),
        (b'i', b'b', 0, 0),
    ),
    (
        ("pg_mcv_list", "text"),
        (10157, 5017, 25),
        (b'i', b'i', 0, 0),
    ),
    (
        ("pg_ndistinct", "bytea"),
        (10152, 3361, 17),
        (b'i', b'b', 0, 0),
    ),
    (
        ("pg_ndistinct", "text"),
        (10153, 3361, 25),
        (b'i', b'i', 0, 0),
    ),
    (
        ("pg_node_tree", "text"),
        (10151, 194, 25),
        (b'i', b'b', 0, 0),
    ),
    (("point", "box"), (10171, 600, 603), (b'a', b'f', 4091, 1)),
    (("polygon", "box"), (10180, 604, 603), (b'e', b'f', 1446, 1)),
    (
        ("polygon", "circle"),
        (10181, 604, 718),
        (b'e', b'f', 1474, 1),
    ),
    (
        ("polygon", "path"),
        (10179, 604, 602),
        (b'a', b'f', 1447, 1),
    ),
    (
        ("polygon", "point"),
        (10178, 604, 600),
        (b'e', b'f', 1540, 1),
    ),
    (("regclass", "int4"), (10080, 2205, 23), (b'a', b'b', 0, 0)),
    (
        ("regclass", "int8"),
        (10079, 2205, 20),
        (b'a', b'f', 1288, 1),
    ),
    (("regclass", "oid"), (10075, 2205, 26), (b'i', b'b', 0, 0)),
    (
        ("regcollation", "int4"),
        (10087, 4191, 23),
        (b'a', b'b', 0, 0),
    ),
    (
        ("regcollation", "int8"),
        (10086, 4191, 20),
        (b'a', b'f', 1288, 1),
    ),
    (
        ("regcollation", "oid"),
        (10082, 4191, 26),
        (b'i', b'b', 0, 0),
    ),
    (("regconfig", "int4"), (10101, 3734, 23), (b'a', b'b', 0, 0)),
    (
        ("regconfig", "int8"),
        (10100, 3734, 20),
        (b'a', b'f', 1288, 1),
    ),
    (("regconfig", "oid"), (10096, 3734, 26), (b'i', b'b', 0, 0)),
    (
        ("regdictionary", "int4"),
        (10108, 3769, 23),
        (b'a', b'b', 0, 0),
    ),
    (
        ("regdictionary", "int8"),
        (10107, 3769, 20),
        (b'a', b'f', 1288, 1),
    ),
    (
        ("regdictionary", "oid"),
        (10103, 3769, 26),
        (b'i', b'b', 0, 0),
    ),
    (
        ("regnamespace", "int4"),
        (10124, 4089, 23),
        (b'a', b'b', 0, 0),
    ),
    (
        ("regnamespace", "int8"),
        (10123, 4089, 20),
        (b'a', b'f', 1288, 1),
    ),
    (
        ("regnamespace", "oid"),
        (10119, 4089, 26),
        (b'i', b'b', 0, 0),
    ),
    (("regoper", "int4"), (10064, 2203, 23), (b'a', b'b', 0, 0)),
    (
        ("regoper", "int8"),
        (10063, 2203, 20),
        (b'a', b'f', 1288, 1),
    ),
    (("regoper", "oid"), (10059, 2203, 26), (b'i', b'b', 0, 0)),
    (
        ("regoper", "regoperator"),
        (10065, 2203, 2204),
        (b'i', b'b', 0, 0),
    ),
    (
        ("regoperator", "int4"),
        (10073, 2204, 23),
        (b'a', b'b', 0, 0),
    ),
    (
        ("regoperator", "int8"),
        (10072, 2204, 20),
        (b'a', b'f', 1288, 1),
    ),
    (
        ("regoperator", "oid"),
        (10068, 2204, 26),
        (b'i', b'b', 0, 0),
    ),
    (
        ("regoperator", "regoper"),
        (10066, 2204, 2203),
        (b'i', b'b', 0, 0),
    ),
    (("regproc", "int4"), (10048, 24, 23), (b'a', b'b', 0, 0)),
    (("regproc", "int8"), (10047, 24, 20), (b'a', b'f', 1288, 1)),
    (("regproc", "oid"), (10043, 24, 26), (b'i', b'b', 0, 0)),
    (
        ("regproc", "regprocedure"),
        (10049, 24, 2202),
        (b'i', b'b', 0, 0),
    ),
    (
        ("regprocedure", "int4"),
        (10057, 2202, 23),
        (b'a', b'b', 0, 0),
    ),
    (
        ("regprocedure", "int8"),
        (10056, 2202, 20),
        (b'a', b'f', 1288, 1),
    ),
    (
        ("regprocedure", "oid"),
        (10052, 2202, 26),
        (b'i', b'b', 0, 0),
    ),
    (
        ("regprocedure", "regproc"),
        (10050, 2202, 24),
        (b'i', b'b', 0, 0),
    ),
    (("regrole", "int4"), (10117, 4096, 23), (b'a', b'b', 0, 0)),
    (
        ("regrole", "int8"),
        (10116, 4096, 20),
        (b'a', b'f', 1288, 1),
    ),
    (("regrole", "oid"), (10112, 4096, 26), (b'i', b'b', 0, 0)),
    (("regtype", "int4"), (10094, 2206, 23), (b'a', b'b', 0, 0)),
    (
        ("regtype", "int8"),
        (10093, 2206, 20),
        (b'a', b'f', 1288, 1),
    ),
    (("regtype", "oid"), (10089, 2206, 26), (b'i', b'b', 0, 0)),
    (("text", "bpchar"), (10125, 25, 1042), (b'i', b'b', 0, 0)),
    (("text", "char"), (10137, 25, 18), (b'a', b'f', 944, 1)),
    (("text", "name"), (10140, 25, 19), (b'i', b'f', 407, 1)),
    (
        ("text", "regclass"),
        (10109, 25, 2205),
        (b'i', b'f', 1079, 1),
    ),
    (("text", "varchar"), (10126, 25, 1043), (b'i', b'b', 0, 0)),
    (("text", "xml"), (10199, 25, 142), (b'e', b'f', 2896, 1)),
    (
        ("time", "interval"),
        (10160, 1083, 1186),
        (b'i', b'f', 1370, 1),
    ),
    (("time", "time"), (10212, 1083, 1083), (b'i', b'f', 1968, 2)),
    (
        ("time", "timetz"),
        (10161, 1083, 1266),
        (b'i', b'f', 2047, 1),
    ),
    (
        ("timestamp", "date"),
        (10162, 1114, 1082),
        (b'a', b'f', 2029, 1),
    ),
    (
        ("timestamp", "time"),
        (10163, 1114, 1083),
        (b'a', b'f', 1316, 1),
    ),
    (
        ("timestamp", "timestamp"),
        (10213, 1114, 1114),
        (b'i', b'f', 1961, 2),
    ),
    (
        ("timestamp", "timestamptz"),
        (10164, 1114, 1184),
        (b'i', b'f', 2028, 1),
    ),
    (
        ("timestamptz", "date"),
        (10165, 1184, 1082),
        (b'a', b'f', 1178, 1),
    ),
    (
        ("timestamptz", "time"),
        (10166, 1184, 1083),
        (b'a', b'f', 2019, 1),
    ),
    (
        ("timestamptz", "timestamp"),
        (10167, 1184, 1114),
        (b'a', b'f', 2027, 1),
    ),
    (
        ("timestamptz", "timestamptz"),
        (10214, 1184, 1184),
        (b'i', b'f', 1967, 2),
    ),
    (
        ("timestamptz", "timetz"),
        (10168, 1184, 1266),
        (b'a', b'f', 1388, 1),
    ),
    (
        ("timetz", "time"),
        (10170, 1266, 1083),
        (b'a', b'f', 2046, 1),
    ),
    (
        ("timetz", "timetz"),
        (10216, 1266, 1266),
        (b'i', b'f', 1969, 2),
    ),
    (
        ("tsrange", "tsmultirange"),
        (10233, 3908, 4533),
        (b'e', b'f', 4287, 1),
    ),
    (
        ("tstzrange", "tstzmultirange"),
        (10234, 3910, 4534),
        (b'e', b'f', 4290, 1),
    ),
    (("varbit", "bit"), (10190, 1562, 1560), (b'i', b'b', 0, 0)),
    (
        ("varbit", "varbit"),
        (10218, 1562, 1562),
        (b'i', b'f', 1687, 3),
    ),
    (
        ("varchar", "bpchar"),
        (10130, 1043, 1042),
        (b'i', b'b', 0, 0),
    ),
    (("varchar", "char"), (10139, 1043, 18), (b'a', b'f', 944, 1)),
    (
        ("varchar", "name"),
        (10142, 1043, 19),
        (b'i', b'f', 1400, 1),
    ),
    (
        ("varchar", "regclass"),
        (10110, 1043, 2205),
        (b'i', b'f', 1079, 1),
    ),
    (("varchar", "text"), (10129, 1043, 25), (b'i', b'b', 0, 0)),
    (
        ("varchar", "varchar"),
        (10211, 1043, 1043),
        (b'i', b'f', 669, 3),
    ),
    (
        ("varchar", "xml"),
        (10204, 1043, 142),
        (b'e', b'f', 2896, 1),
    ),
    (("xid8", "xid"), (10036, 5069, 28), (b'e', b'f', 5071, 1)),
    (("xml", "bpchar"), (10208, 142, 1042), (b'a', b'b', 0, 0)),
    (("xml", "text"), (10198, 142, 25), (b'a', b'b', 0, 0)),
    (("xml", "varchar"), (10203, 142, 1043), (b'a', b'b', 0, 0)),
];
