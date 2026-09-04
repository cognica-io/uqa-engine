\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
SET client_min_messages = warning;

CREATE EXTENSION IF NOT EXISTS dblink;

DO $oracle$
BEGIN
    PERFORM dblink_connect('notification_listener', 'dbname=postgres');
    PERFORM dblink_connect('notification_sender', 'dbname=postgres');
END
$oracle$;

SELECT 'listen|' || dblink_exec('notification_listener', 'LISTEN uqa_notification_session_oracle');
SELECT pid AS sender_pid FROM dblink('notification_sender', 'SELECT pg_backend_pid()') AS sender(pid integer) \gset
SELECT 'sender-pid|' || (:sender_pid > 0)::text;
SELECT 'notify-idle|' || dblink_exec('notification_sender', $$NOTIFY uqa_notification_session_oracle, 'idle'$$);
SELECT 'idle|' || notify_name || '|' || (be_pid = :sender_pid)::text || '|' || extra FROM dblink_get_notify('notification_listener');

SELECT 'begin|' || dblink_exec('notification_listener', 'BEGIN');
SELECT 'notify-deferred|' || dblink_exec('notification_sender', $$NOTIFY uqa_notification_session_oracle, 'deferred'$$);
SELECT 'inside-transaction|' || count(*)::text FROM dblink_get_notify('notification_listener');
SELECT 'commit|' || dblink_exec('notification_listener', 'COMMIT');
SELECT 'after-commit|' || notify_name || '|' || (be_pid = :sender_pid)::text || '|' || extra FROM dblink_get_notify('notification_listener');

DO $oracle$
BEGIN
    PERFORM dblink_disconnect('notification_sender');
    PERFORM dblink_disconnect('notification_listener');
END
$oracle$;
