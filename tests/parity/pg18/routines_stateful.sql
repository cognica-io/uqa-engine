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
