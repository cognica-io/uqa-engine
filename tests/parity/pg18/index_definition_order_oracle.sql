\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
BEGIN;
CREATE TEMP TABLE index_validation(id integer, embedding integer[]);
INSERT INTO index_validation VALUES (1, ARRAY[1,0]), (1, ARRAY[0,1]);
CREATE INDEX occupied_index ON index_validation(id);
CREATE FUNCTION pg_temp.index_definition_probe(command text) RETURNS jsonb LANGUAGE plpgsql AS $oracle$
DECLARE
    succeeded boolean := false;
    state text;
    message text;
    detail text;
    hint text;
BEGIN
    BEGIN
        EXECUTE command;
        succeeded := true;
        RAISE EXCEPTION 'rollback successful probe';
    EXCEPTION WHEN OTHERS THEN
        IF succeeded THEN RETURN jsonb_build_object('state', NULL); END IF;
        GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT, detail = PG_EXCEPTION_DETAIL, hint = PG_EXCEPTION_HINT;
        RETURN jsonb_build_object('state', state, 'message', message, 'detail', nullif(detail, ''), 'hint', nullif(hint, ''));
    END;
END
$oracle$;
SELECT jsonb_build_object('postgresql_version', current_setting('server_version'), 'cases', jsonb_agg(jsonb_build_object('label', label, 'sql', command, 'expected', pg_temp.index_definition_probe(command)) ORDER BY label))
FROM (VALUES
('duplicate-column', 'CREATE INDEX occupied_index ON index_validation(absent)'),
('skip-column', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation(absent)'),
('duplicate-option', 'CREATE INDEX occupied_index ON index_validation(id) WITH(unknown_option=1)'),
('skip-option', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation(id) WITH(unknown_option=1)'),
('duplicate-expression', 'CREATE INDEX occupied_index ON index_validation((random()))'),
('skip-expression', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation((random()))'),
('skip-predicate', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation(id) WHERE random()>0'),
('options-before-column', 'CREATE INDEX occupied_index ON index_validation(absent) WITH(unknown_option=1)'),
('options-before-immutable', 'CREATE INDEX occupied_index ON index_validation((random())) WITH(unknown_option=1)'),
('expression-binding-before-options', 'CREATE INDEX occupied_index ON index_validation((absent+1)) WITH(unknown_option=1)'),
('predicate-before-options', 'CREATE INDEX occupied_index ON index_validation(id) WITH(unknown_option=1) WHERE random()>0'),
('unique-method-before-column', 'CREATE UNIQUE INDEX occupied_index ON index_validation USING gin(absent)'),
('unique-method-before-expression', 'CREATE UNIQUE INDEX occupied_index ON index_validation USING gin((random()))'),
('skip-unique-data', 'CREATE UNIQUE INDEX IF NOT EXISTS occupied_index ON index_validation(id)'),
('duplicate-unique-data', 'CREATE UNIQUE INDEX occupied_index ON index_validation(id)'),
('skip-data-expression', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation((10/(id-id)))'),
('duplicate-data-expression', 'CREATE INDEX occupied_index ON index_validation((10/(id-id)))'),
('missing-table-before-method', 'CREATE INDEX new_index ON missing_index_validation USING nonexistent(id)'),
('valid-skip', 'CREATE INDEX IF NOT EXISTS occupied_index ON index_validation(id)'),
('fillfactor-overflow-reparse', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''0x460000000000000000p-64'')'),
('real-zero-large-exponent', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''0x0p999999999999999999999999'')'),
('real-zero-negative-exponent', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''0x0p-999999999999999999999999'')'),
('real-underflow-rounded-normal', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''0x1.fffffffffffffp-1023'')'),
('real-subnormal-exact-decimal', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''4.940656458412465441765687928682213723650598026143247644255856825006755072702087518652998363616359923797965646954457177309266567103559397963987747960107818781263007131903114045278458171678489821036887186360569987307230500063874091535649843873124733972731696151400317153853980741262385655911710266585566867681870395603106249319452715914924553293054565444011274801297099995419319894090804165633245247571478690147267801593552386115501348035264934720193790268107107491703332226844753335720832431936092382893458368060106011506169809753078342277318329247904982524730776375927247874656084778203734469699533647017972677717585125660551199131504891101451037862738167250955837389733598993664809941164205702637090279242767544565229087538682506419718265533447265625E-324'')'),
('fillfactor-valid', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=70)'),
('fillfactor-rounded', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=70.5)'),
('fillfactor-hex', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''0x46'')'),
('fillfactor-octal', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''0106'')'),
('fillfactor-low', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=9)'),
('fillfactor-overflow', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''2147483648'')'),
('fillfactor-integer-invalid', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''08'')'),
('fillfactor-exponent', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''7e1'')'),
('fillfactor-hex-fraction', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''0x4.6p4'')'),
('fillfactor-hex-exponent', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=''0x46p0'')'),
('fillfactor-bare', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor)'),
('fillfactor-duplicate', 'CREATE INDEX new_index ON index_validation(id) WITH(fillfactor=80,fillfactor=90)'),
('bool-bare', 'CREATE INDEX new_index ON index_validation(id) WITH(deduplicate_items)'),
('bool-false', 'CREATE INDEX new_index ON index_validation(id) WITH(deduplicate_items=false)'),
('bool-prefix', 'CREATE INDEX new_index ON index_validation(id) WITH(deduplicate_items=''of'')'),
('bool-short', 'CREATE INDEX new_index ON index_validation(id) WITH(deduplicate_items=''o'')'),
('bool-space', 'CREATE INDEX new_index ON index_validation(id) WITH(deduplicate_items='' true '')'),
('real-nan', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''NaN'')'),
('real-infinity', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''Infinity'')'),
('real-overflow', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''1e999'')'),
('real-underflow', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''1e-999'')'),
('real-small-normal', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''2.2250738585072014e-308'')'),
('real-underflow-decimal-rounded-normal', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''2.2250738585072012e-308'')'),
('real-hex', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''0x1.fp3'')'),
('real-subnormal-exact', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''0x1p-1074'')'),
('real-subnormal-rounded', 'CREATE INDEX new_index ON index_validation(id) WITH(vacuum_cleanup_index_scale_factor=''5e-324'')'),
('invalid-namespace', 'CREATE INDEX new_index ON index_validation(id) WITH(hello.fillfactor=80)'),
('quoted-option-dot', 'CREATE INDEX new_index ON index_validation(id) WITH("hello.fillfactor"=80)'),
('quoted-option-case', 'CREATE INDEX new_index ON index_validation(id) WITH("FILLFACTOR"=80)'),
('gin-unknown-option', 'CREATE INDEX new_index ON index_validation USING gin(embedding) WITH(unknown_option=1)'),
('gin-pending-low', 'CREATE INDEX new_index ON index_validation USING gin(embedding) WITH(gin_pending_list_limit=63)'),
('gin-bool-invalid', 'CREATE INDEX new_index ON index_validation USING gin(embedding) WITH(fastupdate=''o'')')
) AS probes(label, command);
ROLLBACK;
