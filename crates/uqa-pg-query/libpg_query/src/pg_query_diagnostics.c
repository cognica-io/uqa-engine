#include "pg_query_diagnostics.h"
#include "utils/guc.h"

static __thread PgQueryDiagnosticScope *active_diagnostics = NULL;

void
pg_query_capture_diagnostic(const ErrorData *error)
{
	PgQueryDiagnostic diagnostic;
	int sqlstate = error->sqlerrcode;
	int i;

	if (active_diagnostics == NULL || active_diagnostics->callback == NULL)
		return;
	diagnostic.severity = error->elevel;
	/* Error codes use elog.h's packed six-bit SQLSTATE representation. */
	for (i = 0; i < 5; i++)
	{
		diagnostic.sqlstate[i] = PGUNSIXBIT(sqlstate);
		sqlstate >>= 6;
	}
	diagnostic.sqlstate[5] = '\0';
	diagnostic.message = error->message;
	diagnostic.detail = error->detail;
	diagnostic.hint = error->hint;
	diagnostic.context = error->context;
	diagnostic.cursorpos = error->cursorpos;
	active_diagnostics->callback(active_diagnostics->context, &diagnostic);
}

static void
capture_log(ErrorData *error)
{
	pg_query_capture_diagnostic(error);
	/* The caller receives the diagnostic without process-wide stderr writes. */
	error->output_to_server = false;
}

void
pg_query_begin_diagnostics(PgQueryDiagnosticScope *scope,
	PgQueryDiagnosticCallback callback, void *context)
{
	scope->callback = callback;
	scope->context = context;
	scope->previous = active_diagnostics;
	scope->previous_log_hook = emit_log_hook;
	scope->previous_log_min_messages = log_min_messages;
	active_diagnostics = scope;
	emit_log_hook = capture_log;
	/* Parser NOTICEs, including truncated identifiers, are client diagnostics. */
	log_min_messages = NOTICE;
}

void
pg_query_end_diagnostics(PgQueryDiagnosticScope *scope)
{
	emit_log_hook = scope->previous_log_hook;
	log_min_messages = scope->previous_log_min_messages;
	active_diagnostics = scope->previous;
}
