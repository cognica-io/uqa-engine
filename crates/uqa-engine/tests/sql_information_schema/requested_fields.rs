//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, Value};

#[test]
fn requested_catalog_fields_preserve_aliases_filters_stars_and_whole_rows() {
    let engine = Engine::new();
    engine.sql("CREATE DOMAIN request_type AS regtype DEFAULT 'text'::regtype; CREATE FUNCTION request_echo(n regtype DEFAULT 'text'::regtype) RETURNS regtype LANGUAGE SQL AS 'SELECT n'; CREATE TABLE request_items(id integer, type_ref regtype DEFAULT 'text'::regtype, generated_value integer GENERATED ALWAYS AS (abs(id)) STORED)", &[]).unwrap();
    let rows = engine.sql("SELECT table_name,column_name,data_type FROM information_schema.columns WHERE table_name='request_items' ORDER BY ordinal_position", &[]).unwrap().rows;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1]["column_name"], Value::Str("type_ref".into()));
    let aliased = engine.sql("SELECT c.d FROM information_schema.columns AS c(cat,s,t,col,n,d) WHERE c.t='request_items' AND c.n=2", &[]).unwrap();
    assert_eq!(aliased.rows[0]["d"], Value::Str("'text'::regtype".into()));
    let filtered = engine.sql("SELECT count(*) AS n FROM information_schema.columns WHERE table_name='request_items' AND column_default IS NOT NULL", &[]).unwrap();
    assert_eq!(filtered.rows[0]["n"], Value::Int(1));
    let full = engine.sql("SELECT c.* FROM information_schema.columns c WHERE table_name='request_items' ORDER BY ordinal_position", &[]).unwrap();
    assert_eq!(
        full.rows[1]["column_default"],
        Value::Str("'text'::regtype".into())
    );
    assert_eq!(
        full.rows[2]["generation_expression"],
        Value::Str("abs(id)".into())
    );
    let whole = engine.sql("SELECT row_to_json(c)::text AS entry FROM information_schema.columns c WHERE table_name='request_items' AND column_name='type_ref'", &[]).unwrap();
    let Value::Str(json) = &whole.rows[0]["entry"] else {
        panic!("JSON text");
    };
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["column_default"], "'text'::regtype");
    let joined = engine.sql("SELECT a.column_default,b.generation_expression FROM information_schema.columns a JOIN information_schema.columns b ON a.table_name=b.table_name WHERE a.table_name='request_items' AND a.column_name='type_ref' AND b.column_name='generated_value'", &[]).unwrap();
    assert_eq!(joined.rows.len(), 1);
    assert_eq!(
        joined.rows[0]["column_default"],
        Value::Str("'text'::regtype".into())
    );
    assert_eq!(
        joined.rows[0]["generation_expression"],
        full.rows[2]["generation_expression"]
    );
    let domain = engine.sql("SELECT typdefault,pg_get_expr(typdefaultbin,0) AS expression FROM pg_type WHERE typname='request_type'", &[]).unwrap();
    assert_eq!(
        domain.rows[0]["typdefault"],
        Value::Str("'text'::regtype".into())
    );
    assert_eq!(domain.rows[0]["expression"], domain.rows[0]["typdefault"]);
    let routine = engine.sql("SELECT pronargdefaults,proargdefaults IS NOT NULL AS has_default,pg_get_function_arguments(oid) AS arguments FROM pg_proc WHERE proname='request_echo'", &[]).unwrap();
    assert_eq!(routine.rows[0]["pronargdefaults"], Value::Int(1));
    assert_eq!(routine.rows[0]["has_default"], Value::Bool(true));
    assert_eq!(
        routine.rows[0]["arguments"],
        Value::Str("n regtype DEFAULT 'text'::regtype".into())
    );
}
