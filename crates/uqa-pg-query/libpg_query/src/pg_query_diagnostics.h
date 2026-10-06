#ifndef PG_QUERY_DIAGNOSTICS_H
#define PG_QUERY_DIAGNOSTICS_H

#include "pg_query.h"
#include "postgres.h"
#include "utils/elog.h"

typedef struct PgQueryDiagnosticScope
{
	PgQueryDiagnosticCallback callback;
	void *context;
	struct PgQueryDiagnosticScope *previous;
	emit_log_hook_type previous_log_hook;
	int previous_log_min_messages;
} PgQueryDiagnosticScope;

void pg_query_begin_diagnostics(PgQueryDiagnosticScope *scope,
	PgQueryDiagnosticCallback callback, void *context);
void pg_query_end_diagnostics(PgQueryDiagnosticScope *scope);
void pg_query_capture_diagnostic(const ErrorData *error);

#endif
