//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn returns_setof_with_return_next() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION gen(n int) RETURNS SETOF integer AS $$
         BEGIN
           FOR i IN 1..n LOOP
             RETURN NEXT i * 10;
           END LOOP;
           RETURN;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM gen(3)");
    let values: Vec<Value> = result
        .rows
        .iter()
        .map(|row| row.get("gen").cloned().unwrap())
        .collect();
    assert_eq!(values, vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
    // PG17: reaching the end of a SETOF function without RETURN is
    // fine; the accumulated set is returned.
    exec(
        &eng,
        "CREATE FUNCTION gen2() RETURNS SETOF int AS $$
         BEGIN RETURN NEXT 1; RETURN NEXT 2; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(exec(&eng, "SELECT * FROM gen2()").rows.len(), 2);
}

#[test]
fn returns_table_with_return_query() {
    let eng = engine();
    exec(&eng, "CREATE TABLE items (id INTEGER, label TEXT)");
    exec(
        &eng,
        "INSERT INTO items VALUES (1, 'one'), (2, 'two'), (3, 'three')",
    );
    exec(
        &eng,
        "CREATE FUNCTION list_items(min_id int) RETURNS TABLE(id int, label text) AS $$
         BEGIN
           RETURN QUERY SELECT items.id, items.label FROM items
                        WHERE items.id >= min_id ORDER BY items.id;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM list_items(2)");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[0].get("label"), Some(&Value::Str("two".into())));
    // RETURN NEXT with TABLE columns assigns the column variables.
    exec(
        &eng,
        "CREATE FUNCTION tbl_next(n int) RETURNS TABLE(x int, y text) AS $$
         BEGIN
           x := n; y := 'a'; RETURN NEXT;
           x := n + 1; y := 'b'; RETURN NEXT;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM tbl_next(7)");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[1].get("x"), Some(&Value::Int(8)));
    assert_eq!(result.rows[1].get("y"), Some(&Value::Str("b".into())));
}

#[test]
fn set_valued_function_rejected_in_scalar_context() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION setf() RETURNS SETOF int AS $$
         BEGIN RETURN NEXT 1; END;
         $$ LANGUAGE plpgsql",
    );
    // Documented divergence: PG17 expands SRFs in the select list;
    // the engine rejects them outside FROM with PG's wording for
    // contexts that cannot accept a set.
    let err = exec_err(&eng, "SELECT abs(setf()) AS v");
    assert!(
        err.to_string()
            .contains("set-valued function called in context that cannot accept a set"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------
