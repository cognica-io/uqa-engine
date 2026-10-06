//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared `pg_proc`/`pg_aggregate` signatures used before ordinary aggregate modifiers.
//! `PostgreSQL` `REL_18_4` catalog declarations; polymorphic types remain polymorphic during ranking.

pub(super) const SIGNATURES: &str = "\
array_agg|anyarray|anyarray|a|n|0
array_agg|anynonarray|anyarray|a|n|0
avg|bigint|numeric|a|n|0
avg|double precision|double precision|a|n|0
avg|integer|numeric|a|n|0
avg|interval|interval|a|n|0
avg|numeric|numeric|a|n|0
avg|real|double precision|a|n|0
avg|smallint|numeric|a|n|0
bool_and|boolean|boolean|a|n|0
bool_or|boolean|boolean|a|n|0
count||bigint|a|n|0
count|any|bigint|a|n|0
cume_dist||double precision|w||
cume_dist|any|double precision|a|h|1
dense_rank||bigint|w||
dense_rank|any|bigint|a|h|1
first_value|anyelement|anyelement|w||
json_agg|anyelement|json|a|n|0
json_object_agg|any, any|json|a|n|0
jsonb_agg|anyelement|jsonb|a|n|0
jsonb_object_agg|any, any|jsonb|a|n|0
lag|anycompatible, integer, anycompatible|anycompatible|w||
lag|anyelement|anyelement|w||
lag|anyelement, integer|anyelement|w||
last_value|anyelement|anyelement|w||
lead|anycompatible, integer, anycompatible|anycompatible|w||
lead|anyelement|anyelement|w||
lead|anyelement, integer|anyelement|w||
max|anyarray|anyarray|a|n|0
max|anyenum|anyenum|a|n|0
max|bigint|bigint|a|n|0
max|bytea|bytea|a|n|0
max|character|character|a|n|0
max|date|date|a|n|0
max|double precision|double precision|a|n|0
max|inet|inet|a|n|0
max|integer|integer|a|n|0
max|interval|interval|a|n|0
max|money|money|a|n|0
max|numeric|numeric|a|n|0
max|oid|oid|a|n|0
max|pg_lsn|pg_lsn|a|n|0
max|real|real|a|n|0
max|record|record|a|n|0
max|smallint|smallint|a|n|0
max|text|text|a|n|0
max|tid|tid|a|n|0
max|timestamp without time zone|timestamp without time zone|a|n|0
max|timestamp with time zone|timestamp with time zone|a|n|0
max|time without time zone|time without time zone|a|n|0
max|time with time zone|time with time zone|a|n|0
max|xid8|xid8|a|n|0
min|anyarray|anyarray|a|n|0
min|anyenum|anyenum|a|n|0
min|bigint|bigint|a|n|0
min|bytea|bytea|a|n|0
min|character|character|a|n|0
min|date|date|a|n|0
min|double precision|double precision|a|n|0
min|inet|inet|a|n|0
min|integer|integer|a|n|0
min|interval|interval|a|n|0
min|money|money|a|n|0
min|numeric|numeric|a|n|0
min|oid|oid|a|n|0
min|pg_lsn|pg_lsn|a|n|0
min|real|real|a|n|0
min|record|record|a|n|0
min|smallint|smallint|a|n|0
min|text|text|a|n|0
min|tid|tid|a|n|0
min|timestamp without time zone|timestamp without time zone|a|n|0
min|timestamp with time zone|timestamp with time zone|a|n|0
min|time without time zone|time without time zone|a|n|0
min|time with time zone|time with time zone|a|n|0
min|xid8|xid8|a|n|0
mode|anyelement|anyelement|a|o|0
nth_value|anyelement, integer|anyelement|w||
ntile|integer|integer|w||
percent_rank||double precision|w||
percent_rank|any|double precision|a|h|1
percentile_cont|double precision, double precision|double precision|a|o|1
percentile_cont|double precision[], double precision|double precision[]|a|o|1
percentile_cont|double precision, interval|interval|a|o|1
percentile_cont|double precision[], interval|interval[]|a|o|1
percentile_disc|double precision, anyelement|anyelement|a|o|1
percentile_disc|double precision[], anyelement|anyarray|a|o|1
rank||bigint|w||
rank|any|bigint|a|h|1
row_number||bigint|w||
stddev|bigint|numeric|a|n|0
stddev|double precision|double precision|a|n|0
stddev|integer|numeric|a|n|0
stddev|numeric|numeric|a|n|0
stddev|real|double precision|a|n|0
stddev|smallint|numeric|a|n|0
stddev_pop|bigint|numeric|a|n|0
stddev_pop|double precision|double precision|a|n|0
stddev_pop|integer|numeric|a|n|0
stddev_pop|numeric|numeric|a|n|0
stddev_pop|real|double precision|a|n|0
stddev_pop|smallint|numeric|a|n|0
stddev_samp|bigint|numeric|a|n|0
stddev_samp|double precision|double precision|a|n|0
stddev_samp|integer|numeric|a|n|0
stddev_samp|numeric|numeric|a|n|0
stddev_samp|real|double precision|a|n|0
stddev_samp|smallint|numeric|a|n|0
string_agg|bytea, bytea|bytea|a|n|0
string_agg|text, text|text|a|n|0
sum|bigint|numeric|a|n|0
sum|double precision|double precision|a|n|0
sum|integer|bigint|a|n|0
sum|interval|interval|a|n|0
sum|money|money|a|n|0
sum|numeric|numeric|a|n|0
sum|real|real|a|n|0
sum|smallint|bigint|a|n|0
var_pop|bigint|numeric|a|n|0
var_pop|double precision|double precision|a|n|0
var_pop|integer|numeric|a|n|0
var_pop|numeric|numeric|a|n|0
var_pop|real|double precision|a|n|0
var_pop|smallint|numeric|a|n|0
var_samp|bigint|numeric|a|n|0
var_samp|double precision|double precision|a|n|0
var_samp|integer|numeric|a|n|0
var_samp|numeric|numeric|a|n|0
var_samp|real|double precision|a|n|0
var_samp|smallint|numeric|a|n|0
variance|bigint|numeric|a|n|0
variance|double precision|double precision|a|n|0
variance|integer|numeric|a|n|0
variance|numeric|numeric|a|n|0
variance|real|double precision|a|n|0
variance|smallint|numeric|a|n|0
";
