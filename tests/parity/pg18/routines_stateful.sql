-- Stateful PostgreSQL 18.4 routine parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- Core simple and common polymorphic families.
-- @case create_identity ok
CREATE FUNCTION sf_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_pair ok
CREATE FUNCTION sf_pair(value anyelement, items anyarray) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_compatible ok
CREATE FUNCTION sf_compatible(left_value anycompatible, right_value anycompatible) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_shape_element ok
CREATE FUNCTION sf_shape(elem anyelement) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''element''';
-- @end

-- @case create_shape_array ok
CREATE FUNCTION sf_shape(arr anyarray) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''array''';
-- @end

-- @case identity_bigint rows
SELECT sf_identity(7::bigint) AS value, pg_typeof(sf_identity(7::bigint)) AS value_type;
-- @end

-- @case compatible_promotes_to_bigint rows
SELECT sf_compatible(1::smallint, 2::bigint) AS value, pg_typeof(sf_compatible(1::smallint, 2::bigint)) AS value_type;
-- @end

-- @case named_polymorphic_overloads rows
SELECT sf_shape(elem => ARRAY[1,2]) AS element_choice, sf_shape(arr => ARRAY[1,2]) AS array_choice;
-- @end

-- @case unresolved_polymorphic_null error
SELECT sf_identity(NULL);
-- @end

-- @case inconsistent_simple_family error
SELECT sf_pair(1, ARRAY['x']);
-- @end

-- @case ambiguous_polymorphic_overload error
SELECT sf_shape(ARRAY[1,2]);
-- @end

-- Persisted view binding is observed after a separate UQA process reopens the database.
-- @case create_identity_view ok
CREATE VIEW sf_identity_view AS SELECT sf_identity(7::bigint) AS value;
-- @end

-- @case identity_view_after_reopen rows
SELECT value, pg_typeof(value) AS value_type FROM sf_identity_view;
-- @end

-- Concrete, defaulted, and polymorphic variadic resolution.
-- @case create_pack ok
CREATE FUNCTION sf_pack(VARIADIC items integer[]) RETURNS integer[] LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_variadic_default ok
CREATE FUNCTION sf_variadic_default(VARIADIC items integer[] DEFAULT ARRAY[9]) RETURNS integer[] LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_choose_fixed ok
CREATE FUNCTION sf_choose(value integer) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''fixed''';
-- @end

-- @case create_choose_variadic ok
CREATE FUNCTION sf_choose(VARIADIC items integer[]) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''variadic''';
-- @end

-- @case pack_implicit rows
SELECT sf_pack(1, 2) AS value, pg_typeof(sf_pack(1, 2)) AS value_type;
-- @end

-- @case pack_explicit rows
SELECT sf_pack(VARIADIC ARRAY[3,4]) AS value, pg_typeof(sf_pack(VARIADIC ARRAY[3,4])) AS value_type;
-- @end

-- @case pack_named_explicit rows
SELECT sf_pack(VARIADIC items => ARRAY[5,6]) AS value;
-- @end

-- @case variadic_default_zero rows
SELECT sf_variadic_default() AS value;
-- @end

-- @case variadic_default_named_explicit rows
SELECT sf_variadic_default(VARIADIC items => ARRAY[7,8]) AS value;
-- @end

-- @case fixed_precedes_expanded_variadic rows
SELECT sf_choose(1) AS fixed_choice, sf_choose(1, 2) AS variadic_choice;
-- @end

-- @case pack_empty_without_default error
SELECT sf_pack();
-- @end

-- @case pack_normal_array_is_not_explicit error
SELECT sf_pack(ARRAY[1,2]);
-- @end

-- @case named_variadic_requires_explicit_keyword error
SELECT sf_variadic_default(items => ARRAY[1,2]);
-- @end

-- @case create_zero_fixed ok
CREATE FUNCTION sf_zero() RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''fixed-zero''';
-- @end

-- @case create_zero_default_variadic ok
CREATE FUNCTION sf_zero(VARIADIC items integer[] DEFAULT ARRAY[9]) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''variadic-zero''';
-- @end

-- @case fixed_zero_and_defaulted_variadic_are_ambiguous error
SELECT sf_zero();
-- @end

-- @case create_rank_fixed_array_candidate ok
CREATE FUNCTION sf_rank(value integer[]) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''fixed-array''';
-- @end

-- @case create_rank_expanded_polymorphic_candidate ok
CREATE FUNCTION sf_rank(VARIADIC items anyarray) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''expanded-polymorphic''';
-- @end

-- @case explicit_variadic_keeps_fixed_array_candidate rows
SELECT sf_rank(VARIADIC ARRAY[1,2]) AS selected;
-- @end

-- @case nonexplicit_named_notation_excludes_variadic error
SELECT sf_rank(items => ARRAY[1,2]);
-- @end

-- Polymorphic TABLE, SETOF, PL/pgSQL RETURN NEXT, and CALL paths.
-- @case create_poly_table ok
CREATE FUNCTION sf_poly_table(VARIADIC items anycompatiblearray) RETURNS TABLE(item anycompatible) LANGUAGE SQL IMMUTABLE AS 'SELECT unnest(items)';
-- @end

-- @case create_poly_set ok
CREATE FUNCTION sf_poly_set(VARIADIC items anyarray) RETURNS SETOF anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT unnest(items)';
-- @end

-- @case create_plpgsql_poly_set ok
CREATE FUNCTION sf_plpgsql_poly_set(VARIADIC items anyarray) RETURNS SETOF anyelement LANGUAGE plpgsql IMMUTABLE AS $$ BEGIN RETURN NEXT items[1]; RETURN NEXT items[2]; END $$;
-- @end

-- @case poly_table_implicit rows
SELECT item, pg_typeof(item) AS item_type FROM sf_poly_table(1::smallint, 2::bigint) ORDER BY item;
-- @end

-- @case poly_table_named_explicit rows
SELECT item, pg_typeof(item) AS item_type FROM sf_poly_table(VARIADIC items => ARRAY[3,4]::bigint[]) ORDER BY item;
-- @end

-- @case poly_set_explicit rows
SELECT value, pg_typeof(value) AS value_type FROM sf_poly_set(VARIADIC ARRAY[9,10]::bigint[]) AS result(value) ORDER BY value;
-- @end

-- @case plpgsql_poly_set_implicit rows
SELECT value, pg_typeof(value) AS value_type FROM sf_plpgsql_poly_set(11::bigint, 12::bigint) AS result(value) ORDER BY value;
-- @end

-- @case create_variadic_call_log ok
CREATE TABLE sf_variadic_call_log(sequence SERIAL PRIMARY KEY, items integer[]);
-- @end

-- @case create_variadic_procedure ok
CREATE PROCEDURE sf_record_items(VARIADIC items integer[]) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO sf_variadic_call_log(items) VALUES (items); END $$;
-- @end

-- @case call_variadic_implicit ok
CALL sf_record_items(1, 2);
-- @end

-- @case call_variadic_explicit ok
CALL sf_record_items(VARIADIC ARRAY[3,4]);
-- @end

-- @case call_variadic_named_explicit ok
CALL sf_record_items(VARIADIC items => ARRAY[5,6]);
-- @end

-- @case variadic_call_rows_and_types rows
SELECT sequence, items, pg_typeof(items) AS items_type FROM sf_variadic_call_log ORDER BY sequence;
-- @end

-- Nested overloads and generated columns must retain the substituted concrete type.
-- @case create_generated_kind_integer ok
CREATE FUNCTION sf_generated_kind(value integer) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer''';
-- @end

-- @case create_generated_kind_bigint ok
CREATE FUNCTION sf_generated_kind(value bigint) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''bigint''';
-- @end

-- @case create_generated_poly_table ok
CREATE TABLE sf_generated_poly(source bigint, copied bigint GENERATED ALWAYS AS (sf_identity(source)) STORED, kind text GENERATED ALWAYS AS (sf_generated_kind(sf_identity(source))) STORED);
-- @end

-- @case insert_generated_poly ok
INSERT INTO sf_generated_poly(source) VALUES (42);
-- @end

-- @case generated_poly_rows_and_types rows
SELECT source, copied, kind, pg_typeof(copied) AS copied_type FROM sf_generated_poly;
-- @end

-- PostgreSQL pseudo-type declaration validation boundaries.
-- @case reject_sql_record_input error
CREATE FUNCTION sf_bad_record(value record) RETURNS integer LANGUAGE SQL AS 'SELECT 1';
-- @end

-- @case allow_plpgsql_record_input ok
CREATE FUNCTION sf_record_plpgsql(value record) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $$ BEGIN RETURN 1; END $$;
-- @end

-- @case reject_sql_void_input error
CREATE FUNCTION sf_bad_void(value void) RETURNS integer LANGUAGE SQL AS 'SELECT 1';
-- @end

-- @case reject_sql_internal_input error
CREATE FUNCTION sf_bad_internal(value internal) RETURNS integer LANGUAGE SQL AS 'SELECT 1';
-- @end

-- @case reject_unbound_polymorphic_output error
CREATE FUNCTION sf_bad_return() RETURNS anyelement LANGUAGE SQL AS 'SELECT NULL';
-- @end

-- @case reject_nonarray_variadic error
CREATE FUNCTION sf_bad_variadic(VARIADIC value integer) RETURNS integer LANGUAGE SQL AS 'SELECT 1';
-- @end

-- @case reject_sql_standard_polymorphic_body error
CREATE FUNCTION sf_bad_atomic(value anyelement) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1; END;
-- @end

-- @case plpgsql_record_catalog_identity rows
SELECT proargtypes, prorettype, (SELECT count(*) FROM pg_catalog.pg_proc WHERE proname IN ('sf_bad_record', 'sf_bad_void', 'sf_bad_internal', 'sf_bad_return', 'sf_bad_variadic', 'sf_bad_atomic') AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema())) AS rejected_catalog_rows FROM pg_catalog.pg_proc WHERE proname = 'sf_record_plpgsql' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema());
-- @end

-- User pg_proc identity: input identity, OUT/TABLE modes, pseudo OIDs, defaults, and variadic element OIDs.
-- @case create_catalog_variadic_out ok
CREATE FUNCTION sf_catalog(prefix integer, VARIADIC items integer[] DEFAULT ARRAY[1,2], OUT total bigint) LANGUAGE SQL AS 'SELECT 1::bigint';
-- @end

-- @case create_catalog_procedure ok
CREATE PROCEDURE sf_catalog_proc(OUT y text, IN a integer DEFAULT 7) LANGUAGE plpgsql AS $$ BEGIN y := 'x'; END $$;
-- @end

-- @case user_pg_proc_identity rows
SELECT proname, prokind, provariadic, pronargs, pronargdefaults, prorettype, proretset, proargtypes, proallargtypes, proargmodes, proargnames FROM pg_catalog.pg_proc WHERE proname IN ('sf_catalog', 'sf_catalog_proc', 'sf_pack', 'sf_poly_set', 'sf_poly_table') AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema()) ORDER BY proname;
-- @end

-- ALTER FUNCTION/ROUTINE exact, omitted, ambiguous, kind, and persistence lifecycle.
-- @case create_alter_exact_integer ok
CREATE FUNCTION sf_alter_exact(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''integer-body''';
-- @end

-- @case create_alter_exact_bigint ok
CREATE FUNCTION sf_alter_exact(value bigint) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''bigint-body''';
-- @end

-- @case alter_exact_integer ok
ALTER FUNCTION sf_alter_exact(integer) IMMUTABLE STRICT;
-- @end

-- @case alter_exact_catalog rows
SELECT proargtypes, provolatile, proisstrict FROM pg_catalog.pg_proc WHERE proname = 'sf_alter_exact' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema()) ORDER BY proargtypes::text;
-- @end

-- @case altered_bodies_preserved rows
SELECT sf_alter_exact(7) AS integer_body, sf_alter_exact(7::bigint) AS bigint_body;
-- @end

-- @case create_alter_unique ok
CREATE FUNCTION sf_alter_unique(value integer) RETURNS integer LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT $1 + 5';
-- @end

-- @case alter_unique_without_signature ok
ALTER FUNCTION sf_alter_unique STABLE STRICT;
-- @end

-- @case alter_unique_after_reopen rows
SELECT sf_alter_unique(7) AS value, provolatile, proisstrict FROM pg_catalog.pg_proc WHERE proname = 'sf_alter_unique' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema());
-- @end

-- @case create_alter_zero ok
CREATE FUNCTION sf_alter_zero() RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''zero-body''';
-- @end

-- @case create_alter_zero_integer ok
CREATE FUNCTION sf_alter_zero(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''integer-body''';
-- @end

-- @case alter_omitted_signature_ambiguous error
ALTER FUNCTION sf_alter_zero IMMUTABLE;
-- @end

-- @case alter_explicit_empty_signature ok
ALTER FUNCTION sf_alter_zero() IMMUTABLE STRICT;
-- @end

-- @case alter_zero_bodies_and_attributes rows
SELECT proargtypes, provolatile, proisstrict, sf_alter_zero() AS zero_body, sf_alter_zero(7) AS integer_body FROM pg_catalog.pg_proc WHERE proname = 'sf_alter_zero' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema()) ORDER BY pronargs;
-- @end

-- @case create_alter_only_procedure ok
CREATE PROCEDURE sf_alter_only_procedure(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
-- @end

-- @case create_alter_only_function ok
CREATE FUNCTION sf_alter_only_function(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT $1';
-- @end

-- @case alter_function_wrong_kind error
ALTER FUNCTION sf_alter_only_procedure(integer) IMMUTABLE;
-- @end

-- @case alter_procedure_wrong_kind error
ALTER PROCEDURE sf_alter_only_function(integer) STABLE;
-- @end

-- @case alter_missing_function error
ALTER FUNCTION sf_alter_missing(integer) IMMUTABLE;
-- @end

-- @case alter_procedure_function_attribute_error error
ALTER PROCEDURE sf_alter_only_procedure(integer) STABLE;
-- @end

-- @case create_alter_routine_function ok
CREATE FUNCTION sf_alter_neutral(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''function-body''';
-- @end

-- @case create_alter_routine_procedure ok
CREATE PROCEDURE sf_alter_neutral(value text) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
-- @end

-- @case alter_routine_omitted_ambiguous error
ALTER ROUTINE sf_alter_neutral VOLATILE;
-- @end

-- @case alter_routine_exact_function ok
ALTER ROUTINE sf_alter_neutral(integer) IMMUTABLE STRICT;
-- @end

-- @case alter_routine_procedure_attribute_error error
ALTER ROUTINE sf_alter_neutral(text) STABLE CALLED ON NULL INPUT;
-- @end

-- @case alter_routine_catalog_and_body rows
SELECT proname, proargtypes, prokind, provolatile, proisstrict FROM pg_catalog.pg_proc WHERE proname IN ('sf_alter_neutral', 'sf_alter_only_function', 'sf_alter_only_procedure') AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema()) ORDER BY proname, proargtypes::text;
-- @end

-- Bounded DROP CASCADE removes exact generated/view dependents and preserves unrelated objects and overloads.
-- @case create_cascade_polymorphic ok
CREATE FUNCTION sf_cascade_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_cascade_bigint_overload ok
CREATE FUNCTION sf_cascade_identity(value bigint) RETURNS bigint LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_cascade_source ok
CREATE TABLE sf_cascade_source(x text, y text GENERATED ALWAYS AS (sf_cascade_identity(x)) STORED);
-- @end

-- @case create_cascade_direct_view ok
CREATE VIEW sf_cascade_direct AS SELECT sf_cascade_identity(x) AS value FROM sf_cascade_source;
-- @end

-- @case create_cascade_nested_view ok
CREATE VIEW sf_cascade_nested AS SELECT value FROM sf_cascade_direct;
-- @end

-- @case create_unrelated_function ok
CREATE FUNCTION sf_keep(value integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT $1';
-- @end

-- @case create_unrelated_view ok
CREATE VIEW sf_keep_view AS SELECT sf_keep(99) AS value;
-- @end

-- @case drop_polymorphic_restrict_has_dependents error
DROP FUNCTION sf_cascade_identity(anyelement);
-- @end

-- @case drop_polymorphic_cascade ok
DROP FUNCTION sf_cascade_identity(anyelement) CASCADE;
-- @end

-- @case cascade_removed_generated_column rows
SELECT column_name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'sf_cascade_source' ORDER BY ordinal_position;
-- @end

-- @case cascade_preserved_exact_overload_and_unrelated_view rows
SELECT sf_cascade_identity(7::bigint) AS overload_value, value AS unrelated_value FROM sf_keep_view;
-- @end

-- @case cascade_catalog_is_bounded rows
SELECT proargtypes FROM pg_catalog.pg_proc WHERE proname = 'sf_cascade_identity' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema());
-- @end

-- @case cascade_removed_direct_view error
SELECT * FROM sf_cascade_direct;
-- @end

-- @case cascade_removed_nested_view error
SELECT * FROM sf_cascade_nested;
-- @end

-- @case cascade_removed_polymorphic_text_call error
SELECT sf_cascade_identity('x'::text);
-- @end

-- @case cascade_source_remains_writable ok
INSERT INTO sf_cascade_source(x) VALUES ('eight');
-- @end

-- @case cascade_source_rows rows
SELECT x FROM sf_cascade_source;
-- @end

-- @case create_cascade_procedure ok
CREATE PROCEDURE sf_cascade_procedure(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
-- @end

-- @case drop_procedure_cascade ok
DROP PROCEDURE sf_cascade_procedure(integer) CASCADE;
-- @end

-- @case dropped_procedure_call error
CALL sf_cascade_procedure(1);
-- @end

-- SQL-standard query bodies own exact routine dependencies; string bodies stay dynamic.
-- @case create_dependency_base ok
CREATE FUNCTION sf_dependency_base(value integer) RETURNS integer RETURN value + 1;
-- @end

-- @case create_dependency_middle ok
CREATE FUNCTION sf_dependency_middle(value integer) RETURNS integer RETURN sf_dependency_base(value);
-- @end

-- @case create_dependency_leaf ok
CREATE FUNCTION sf_dependency_leaf(value integer) RETURNS integer RETURN sf_dependency_middle(value);
-- @end

-- @case create_dependency_procedure ok
CREATE PROCEDURE sf_dependency_procedure(value integer) LANGUAGE SQL BEGIN ATOMIC SELECT sf_dependency_leaf(value); END;
-- @end

-- @case routine_dependency_restrict error
DROP FUNCTION sf_dependency_base(integer) RESTRICT;
-- @end

-- @case routine_dependency_cascade ok
DROP FUNCTION sf_dependency_base(integer) CASCADE;
-- @end

-- @case routine_dependency_catalog_empty rows
SELECT proname FROM pg_catalog.pg_proc WHERE proname IN ('sf_dependency_base', 'sf_dependency_middle', 'sf_dependency_leaf', 'sf_dependency_procedure') AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = current_schema()) ORDER BY proname;
-- @end

-- @case create_multi_dependency_base ok
CREATE FUNCTION sf_multi_dependency_base(value integer) RETURNS integer RETURN value + 1;
-- @end

-- @case create_multi_dependency_leaf ok
CREATE FUNCTION sf_multi_dependency_leaf(value integer) RETURNS integer RETURN sf_multi_dependency_base(value);
-- @end

-- @case drop_multi_dependency_graph_restrict ok
DROP FUNCTION sf_multi_dependency_base(integer), sf_multi_dependency_leaf(integer) RESTRICT;
-- @end

-- @case create_dynamic_dependency_base ok
CREATE FUNCTION sf_dynamic_dependency_base(value integer) RETURNS integer RETURN value + 1;
-- @end

-- @case create_dynamic_dependency_leaf ok
CREATE FUNCTION sf_dynamic_dependency_leaf(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT sf_dynamic_dependency_base($1)';
-- @end

-- @case drop_dynamic_dependency_base_restrict ok
DROP FUNCTION sf_dynamic_dependency_base(integer) RESTRICT;
-- @end

-- @case dynamic_dependency_call_after_drop error
SELECT sf_dynamic_dependency_leaf(1);
-- @end

-- Positional SQL-standard parameters retain their declared types while overload dependencies bind.
-- @case create_positional_dependency_integer ok
CREATE FUNCTION sf_positional_dependency(value integer) RETURNS integer RETURN value + 1;
-- @end

-- @case create_positional_dependency_bigint ok
CREATE FUNCTION sf_positional_dependency(value bigint) RETURNS bigint RETURN value + 2;
-- @end

-- @case create_positional_dependency_leaf ok
CREATE FUNCTION sf_positional_dependency_leaf(value integer) RETURNS integer RETURN sf_positional_dependency($1);
-- @end

-- @case positional_dependency_result rows
SELECT sf_positional_dependency_leaf(1) AS value;
-- @end

-- @case drop_unreferenced_positional_overload ok
DROP FUNCTION sf_positional_dependency(bigint) RESTRICT;
-- @end

-- @case positional_dependency_restrict error
DROP FUNCTION sf_positional_dependency(integer) RESTRICT;
-- @end

-- @case positional_dependency_cascade ok
DROP FUNCTION sf_positional_dependency(integer) CASCADE;
-- @end

-- PL/pgSQL FOREACH traverses true arrays in storage order, retains slice bounds, evaluates its expression once, updates FOUND, and preserves PostgreSQL's validation order.
-- @case create_foreach_elements ok
CREATE FUNCTION sf_foreach_elements(items integer[]) RETURNS text LANGUAGE plpgsql AS $$ DECLARE item integer; output text := ''; BEGIN PERFORM 1; FOREACH item SLICE 0 IN ARRAY items LOOP output := output || coalesce(item::text, 'NULL') || ','; END LOOP; RETURN output || 'found=' || FOUND::text; END $$;
-- @end

-- @case foreach_elements_storage_order rows
SELECT sf_foreach_elements('[0:1][5:6]={{1,2},{3,4}}'::integer[]) AS value;
-- @end

-- @case foreach_empty_resets_found rows
SELECT sf_foreach_elements(ARRAY[]::integer[]) AS value;
-- @end

-- @case create_foreach_slice ok
CREATE FUNCTION sf_foreach_slice(items integer[]) RETURNS text LANGUAGE plpgsql AS $$ DECLARE item integer[]; output text := ''; BEGIN FOREACH item SLICE 1 IN ARRAY items LOOP output := output || array_dims(item) || '=' || item::text || ';'; END LOOP; RETURN output || 'found=' || FOUND::text; END $$;
-- @end

-- @case foreach_slice_bounds rows
SELECT sf_foreach_slice('[0:1][5:6]={{1,2},{3,4}}'::integer[]) AS value;
-- @end

-- @case create_foreach_source_log ok
CREATE TABLE sf_foreach_source_log(value integer);
-- @end

-- @case create_foreach_source ok
CREATE FUNCTION sf_foreach_source() RETURNS integer[] LANGUAGE plpgsql VOLATILE AS $$ BEGIN INSERT INTO sf_foreach_source_log VALUES (1); RETURN ARRAY[1,2,3]; END $$;
-- @end

-- @case create_foreach_subquery_sum ok
CREATE FUNCTION sf_foreach_subquery_sum() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE item integer; total integer := 0; BEGIN FOREACH item IN ARRAY (SELECT sf_foreach_source()) LOOP total := total + item; END LOOP; RETURN total; END $$;
-- @end

-- @case foreach_subquery_result rows
SELECT sf_foreach_subquery_sum() AS value;
-- @end

-- @case foreach_expression_evaluated_once rows
SELECT count(*) AS calls FROM sf_foreach_source_log;
-- @end

-- @case create_foreach_null ok
CREATE FUNCTION sf_foreach_null(items integer[]) RETURNS integer LANGUAGE plpgsql AS $$ DECLARE item integer; BEGIN FOREACH item IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case foreach_null_expression error
SELECT sf_foreach_null(NULL::integer[]);
-- @end

-- @case create_foreach_nonarray ok
CREATE FUNCTION sf_foreach_nonarray() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE item integer; BEGIN FOREACH item IN ARRAY 42 LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case foreach_nonarray_expression error
SELECT sf_foreach_nonarray();
-- @end

-- @case create_foreach_slice_scalar ok
CREATE FUNCTION sf_foreach_slice_scalar(items integer[]) RETURNS integer LANGUAGE plpgsql AS $$ DECLARE item integer; BEGIN FOREACH item SLICE 1 IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case foreach_empty_dimension_precedes_target error
SELECT sf_foreach_slice_scalar(ARRAY[]::integer[]);
-- @end

-- @case foreach_slice_requires_array_target error
SELECT sf_foreach_slice_scalar(ARRAY[1,2]);
-- @end

-- @case create_foreach_array_element ok
CREATE FUNCTION sf_foreach_array_element(items integer[]) RETURNS integer LANGUAGE plpgsql AS $$ DECLARE item integer[]; BEGIN FOREACH item IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case foreach_element_requires_scalar_target error
SELECT sf_foreach_array_element(ARRAY[]::integer[]);
-- @end

-- Static, dynamic, and bound-cursor query FOR loops share PostgreSQL's portal lifecycle while retaining their distinct prefetch and expression-evaluation behavior.
-- @case create_query_for_values ok
CREATE TABLE sf_query_for_values(value integer);
-- @end

-- @case insert_query_for_values ok
INSERT INTO sf_query_for_values VALUES (1), (2), (3), (4);
-- @end

-- @case create_query_for_eval_log ok
CREATE TABLE sf_query_for_eval_log(kind text);
-- @end

-- @case create_query_for_text ok
CREATE FUNCTION sf_query_for_text() RETURNS text LANGUAGE plpgsql VOLATILE AS $$ BEGIN INSERT INTO sf_query_for_eval_log VALUES ('text'); RETURN 'SELECT value FROM sf_query_for_values WHERE value <= $1 ORDER BY value'; END $$;
-- @end

-- @case create_query_for_limit ok
CREATE FUNCTION sf_query_for_limit() RETURNS integer LANGUAGE plpgsql VOLATILE AS $$ BEGIN INSERT INTO sf_query_for_eval_log VALUES ('parameter'); RETURN 3; END $$;
-- @end

-- @case create_dynamic_query_for_report ok
CREATE FUNCTION sf_dynamic_query_for_report() RETURNS text LANGUAGE plpgsql AS $$ DECLARE loop_row record; output text := ''; BEGIN PERFORM 1 WHERE false; FOR loop_row IN EXECUTE sf_query_for_text() USING sf_query_for_limit() LOOP output := output || loop_row.value || ':' || FOUND || ','; END LOOP; RETURN output || 'last=' || loop_row.value || '|found=' || FOUND; END $$;
-- @end

-- @case dynamic_query_for_result rows
SELECT sf_dynamic_query_for_report() AS value;
-- @end

-- @case dynamic_query_for_evaluates_once rows
SELECT count(*) FILTER (WHERE kind = 'text') AS text_calls, count(*) FILTER (WHERE kind = 'parameter') AS parameter_calls FROM sf_query_for_eval_log;
-- @end

-- @case create_static_query_for_sequence ok
CREATE SEQUENCE sf_static_query_for_sequence START WITH 1;
-- @end

-- @case create_dynamic_query_for_sequence ok
CREATE SEQUENCE sf_dynamic_query_for_sequence START WITH 1;
-- @end

-- @case create_cursor_query_for_sequence ok
CREATE SEQUENCE sf_cursor_query_for_sequence START WITH 1;
-- @end

-- @case create_query_for_prefetch_report ok
CREATE FUNCTION sf_query_for_prefetch_report() RETURNS text LANGUAGE plpgsql AS $$ DECLARE loop_row record; bound_rows CURSOR FOR SELECT nextval('sf_cursor_query_for_sequence') AS value FROM generate_series(1, 100); BEGIN FOR loop_row IN SELECT nextval('sf_static_query_for_sequence') AS value FROM generate_series(1, 100) LOOP EXIT; END LOOP; FOR loop_row IN EXECUTE 'SELECT nextval(''sf_dynamic_query_for_sequence'') AS value FROM generate_series(1, 100)' LOOP EXIT; END LOOP; FOR loop_row IN bound_rows LOOP EXIT; END LOOP; RETURN currval('sf_static_query_for_sequence') || ',' || currval('sf_dynamic_query_for_sequence') || ',' || currval('sf_cursor_query_for_sequence'); END $$;
-- @end

-- @case query_for_prefetch_counts rows
SELECT sf_query_for_prefetch_report() AS value;
-- @end

-- @case create_bound_cursor_for_report ok
CREATE FUNCTION sf_bound_cursor_for_report(low_value integer, high_value integer) RETURNS text LANGUAGE plpgsql AS $$ DECLARE loop_row text := 'outer'; bound_rows CURSOR (low_bound integer, high_bound integer) FOR SELECT value FROM sf_query_for_values WHERE value BETWEEN low_bound AND high_bound ORDER BY value; output text := ''; BEGIN PERFORM 1 WHERE false; FOR loop_row IN bound_rows(high_bound => high_value, low_bound => low_value) LOOP output := output || loop_row.value || ':' || FOUND || ','; END LOOP; RETURN output || 'found=' || FOUND || '|outer=' || loop_row || '|null=' || (bound_rows IS NULL); END $$;
-- @end

-- @case bound_cursor_for_named_arguments rows
SELECT sf_bound_cursor_for_report(2, 3) AS value;
-- @end

-- @case create_dynamic_query_for_null ok
CREATE FUNCTION sf_dynamic_query_for_null() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE loop_row record; BEGIN FOR loop_row IN EXECUTE NULL::text LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case dynamic_query_for_null error
SELECT sf_dynamic_query_for_null();
-- @end

-- @case create_dynamic_query_for_multi ok
CREATE FUNCTION sf_dynamic_query_for_multi() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE loop_row record; BEGIN FOR loop_row IN EXECUTE 'SELECT 1; SELECT 2' LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case dynamic_query_for_multi error
SELECT sf_dynamic_query_for_multi();
-- @end

-- @case create_dynamic_query_for_inserted ok
CREATE TABLE sf_dynamic_query_for_inserted(value integer);
-- @end

-- @case create_dynamic_query_for_nonreturning ok
CREATE FUNCTION sf_dynamic_query_for_nonreturning() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE loop_row record; BEGIN FOR loop_row IN EXECUTE 'INSERT INTO sf_dynamic_query_for_inserted VALUES (9)' LOOP NULL; END LOOP; RETURN 0; END $$;
-- @end

-- @case dynamic_query_for_nonreturning error
SELECT sf_dynamic_query_for_nonreturning();
-- @end

-- @case dynamic_query_for_nonreturning_is_execution_free rows
SELECT count(*) AS rows FROM sf_dynamic_query_for_inserted;
-- @end

-- @case create_bound_cursor_for_pinned_close ok
CREATE FUNCTION sf_bound_cursor_for_pinned_close() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE bound_rows CURSOR FOR SELECT value FROM sf_query_for_values ORDER BY value; BEGIN bound_rows := 'sf_query_for_pinned'; FOR loop_row IN bound_rows LOOP CLOSE bound_rows; RETURN loop_row.value; END LOOP; RETURN 0; END $$;
-- @end

-- @case bound_cursor_for_pinned_close error
SELECT sf_bound_cursor_for_pinned_close();
-- @end

-- @case create_bound_cursor_for_reuse ok
CREATE FUNCTION sf_bound_cursor_for_reuse() RETURNS text LANGUAGE plpgsql AS $$ DECLARE bound_rows CURSOR FOR SELECT value FROM sf_query_for_values ORDER BY value; output text := ''; BEGIN bound_rows := 'sf_query_for_reuse'; FOR loop_row IN bound_rows LOOP output := output || loop_row.value; EXIT; END LOOP; RETURN output || ':' || bound_rows::text; END $$;
-- @end

-- @case bound_cursor_for_closes_and_reuses_name rows
SELECT sf_bound_cursor_for_reuse() AS first_call, sf_bound_cursor_for_reuse() AS second_call;
-- @end

-- PL/pgSQL ASSERT uses assignment-style Boolean coercion, leaves diagnostics unchanged, evaluates messages only on failure, and preserves sequence effects across exception rollback.
-- @case create_assert_true_condition_sequence ok
CREATE SEQUENCE sf_assert_true_condition_sequence START WITH 1;
-- @end

-- @case create_assert_true_message_sequence ok
CREATE SEQUENCE sf_assert_true_message_sequence START WITH 1;
-- @end

-- @case create_assert_true_report ok
CREATE FUNCTION sf_assert_true_report() RETURNS text LANGUAGE plpgsql AS $$ DECLARE before_found boolean; before_count integer; after_count integer; BEGIN PERFORM 1; before_found := FOUND; GET DIAGNOSTICS before_count = ROW_COUNT; ASSERT nextval('sf_assert_true_condition_sequence') > 0, nextval('sf_assert_true_message_sequence'); GET DIAGNOSTICS after_count = ROW_COUNT; RETURN before_found || ':' || FOUND || ':' || before_count || ':' || after_count; END $$;
-- @end

-- @case assert_true_preserves_state rows
SELECT sf_assert_true_report() AS value;
-- @end

-- @case assert_true_message_is_lazy rows
SELECT sequencename, last_value FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename IN ('sf_assert_true_condition_sequence', 'sf_assert_true_message_sequence') ORDER BY sequencename;
-- @end

-- @case create_assert_false_condition_sequence ok
CREATE SEQUENCE sf_assert_false_condition_sequence START WITH 1;
-- @end

-- @case create_assert_false_message_sequence ok
CREATE SEQUENCE sf_assert_false_message_sequence START WITH 1;
-- @end

-- @case create_assert_caught_report ok
CREATE FUNCTION sf_assert_caught_report() RETURNS text LANGUAGE plpgsql AS $$ BEGIN ASSERT nextval('sf_assert_false_condition_sequence') < 0, nextval('sf_assert_false_message_sequence'); RETURN 'missed'; EXCEPTION WHEN assert_failure THEN RETURN SQLSTATE || ':' || SQLERRM; END $$;
-- @end

-- @case assert_named_handler_result rows
SELECT sf_assert_caught_report() AS value;
-- @end

-- @case assert_exception_preserves_sequences rows
SELECT sequencename, last_value FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename IN ('sf_assert_false_condition_sequence', 'sf_assert_false_message_sequence') ORDER BY sequencename;
-- @end

-- @case create_assert_message_report ok
CREATE FUNCTION sf_assert_message_report() RETURNS text LANGUAGE plpgsql AS $$ DECLARE output text := ''; BEGIN BEGIN ASSERT false; EXCEPTION WHEN assert_failure THEN output := SQLSTATE || ':' || SQLERRM; END; BEGIN ASSERT false, 'custom failure'; EXCEPTION WHEN assert_failure THEN output := output || '|' || SQLERRM; END; BEGIN ASSERT NULL::boolean, NULL::text; EXCEPTION WHEN assert_failure THEN output := output || '|' || SQLERRM; END; BEGIN ASSERT false, 42; EXCEPTION WHEN assert_failure THEN output := output || '|' || SQLERRM; END; RETURN output; END $$;
-- @end

-- @case assert_messages_and_null_condition rows
SELECT sf_assert_message_report() AS value;
-- @end

-- @case create_assert_others_report ok
CREATE FUNCTION sf_assert_others_report() RETURNS text LANGUAGE plpgsql AS $$ BEGIN BEGIN ASSERT false; EXCEPTION WHEN OTHERS THEN RETURN 'caught'; END; RETURN 'missed'; END $$;
-- @end

-- @case assert_failure_excluded_from_others error
SELECT sf_assert_others_report();
-- @end

-- @case create_assert_off_condition_sequence ok
CREATE SEQUENCE sf_assert_off_condition_sequence START WITH 1;
-- @end

-- @case create_assert_off_message_sequence ok
CREATE SEQUENCE sf_assert_off_message_sequence START WITH 1;
-- @end

-- @case create_assert_disabled_report ok
CREATE FUNCTION sf_assert_disabled_report() RETURNS integer LANGUAGE plpgsql SET plpgsql.check_asserts = off AS $$ BEGIN ASSERT nextval('sf_assert_off_condition_sequence') < 0, nextval('sf_assert_off_message_sequence'); RETURN 7; END $$;
-- @end

-- @case assert_disabled_skips_expressions rows
SELECT sf_assert_disabled_report() AS value;
-- @end

-- @case assert_disabled_leaves_sequences_uncalled rows
SELECT sequencename, last_value FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename IN ('sf_assert_off_condition_sequence', 'sf_assert_off_message_sequence') ORDER BY sequencename;
-- @end

-- @case create_assert_setting_report ok
CREATE FUNCTION sf_assert_setting_report() RETURNS text LANGUAGE plpgsql AS $$ BEGIN RETURN (SELECT setting || '|' || category || '|' || short_desc || '|' || context || '|' || vartype || '|' || source || '|' || boot_val || '|' || reset_val FROM pg_catalog.pg_settings WHERE name = 'plpgsql.check_asserts'); END $$;
-- @end

-- @case assert_setting_metadata rows
SELECT sf_assert_setting_report() AS value;
-- @end

-- @case assert_setting_accepts_boolean_prefix ok
LOAD 'plpgsql'; SET plpgsql.check_asserts = of;
-- @end

-- @case assert_setting_rejects_ambiguous_prefix error
LOAD 'plpgsql'; SET plpgsql.check_asserts = o;
-- @end

-- @case create_plpgsql_boolean_coercion_report ok
CREATE FUNCTION sf_plpgsql_boolean_coercion_report() RETURNS text LANGUAGE plpgsql AS $$ BEGIN IF 'false' THEN RETURN 'bad'; ELSIF 'yes' THEN RETURN 'ok'; END IF; RETURN 'missed'; END $$;
-- @end

-- @case plpgsql_boolean_coercion_result rows
SELECT sf_plpgsql_boolean_coercion_report() AS value;
-- @end
