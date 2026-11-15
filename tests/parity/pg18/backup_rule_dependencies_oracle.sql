-- Run after backup_rule_bindings_oracle.sql; observe only column and user-routine dependencies.
SELECT string_agg(reference, E'\n' ORDER BY reference) AS dependencies
FROM (
    SELECT DISTINCT 'column:' || r.rulename || ':' || c.relname || '.' || a.attname AS reference
    FROM pg_depend d
    JOIN pg_rewrite r ON d.classid='pg_rewrite'::regclass AND d.objid=r.oid
    JOIN pg_class owner ON owner.oid=r.ev_class
    JOIN pg_namespace owner_ns ON owner_ns.oid=owner.relnamespace
    JOIN pg_class c ON d.refclassid='pg_class'::regclass AND d.refobjid=c.oid
    JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=d.refobjsubid
    WHERE owner_ns.nspname='public' AND owner.relname='items' AND d.deptype='n' AND d.refobjsubid>0
    UNION
    SELECT DISTINCT 'routine:' || r.rulename || ':' || p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')'
    FROM pg_depend d
    JOIN pg_rewrite r ON d.classid='pg_rewrite'::regclass AND d.objid=r.oid
    JOIN pg_class owner ON owner.oid=r.ev_class
    JOIN pg_namespace owner_ns ON owner_ns.oid=owner.relnamespace
    JOIN pg_proc p ON d.refclassid='pg_proc'::regclass AND d.refobjid=p.oid
    JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE owner_ns.nspname='public' AND owner.relname='items' AND n.nspname='public' AND d.deptype='n'
) entries;
