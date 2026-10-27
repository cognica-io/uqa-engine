#include "pg_query.h"
#include "pg_query_internal.h"

#include <mb/pg_wchar.h>
#include <utils/memutils.h>
#include <utils/memdebug.h>

#include "pg_query_thread.h"

#include <signal.h>

const char* progname = "pg_query";

__thread sig_atomic_t pg_query_initialized = 0;

#ifdef PG_QUERY_HAVE_THREAD_CLEANUP
static PgQueryThreadKey pg_query_thread_exit_key;
static PgQueryThreadOnce pg_query_thread_exit_key_once = PG_QUERY_THREAD_ONCE_INIT;
static int pg_query_thread_exit_key_error;
static void pg_query_create_thread_exit_key(void);
static void PG_QUERY_THREAD_CALLBACK pg_query_thread_exit(void *key);
#endif

void pg_query_init(void)
{
	if (pg_query_initialized != 0) return;
	pg_query_initialized = 1;

	MemoryContextInit();
	SetDatabaseEncoding(PG_UTF8);

#ifdef PG_QUERY_HAVE_THREAD_CLEANUP
	if (pg_query_thread_once(&pg_query_thread_exit_key_once, pg_query_create_thread_exit_key) != 0 ||
		pg_query_thread_exit_key_error != 0 ||
		pg_query_thread_setspecific(pg_query_thread_exit_key, TopMemoryContext) != 0)
		elog(ERROR, "could not register parser thread cleanup");
#endif
}

void pg_query_free_top_memory_context(MemoryContext context)
{
	Assert(MemoryContextIsValid(context));

	/*
	 * After this, no memory contexts are valid anymore, so ensure that
	 * the current context is the top-level context.
	 */
	Assert(TopMemoryContext == CurrentMemoryContext);

	MemoryContextDeleteChildren(context);

	context->methods->delete_context(context);

	VALGRIND_DESTROY_MEMPOOL(context);

	/* Deleting the top context can add it to the allocator freelist. */
	AllocSetDeleteFreeList();

	/* Reset pointers */
	TopMemoryContext = NULL;
	CurrentMemoryContext = NULL;
	ErrorContext = NULL;
	pg_query_initialized = 0;
}

#ifdef PG_QUERY_HAVE_THREAD_CLEANUP
static void pg_query_create_thread_exit_key(void)
{
	pg_query_thread_exit_key_error = pg_query_thread_key_create(&pg_query_thread_exit_key, pg_query_thread_exit);
}

static void PG_QUERY_THREAD_CALLBACK pg_query_thread_exit(void *key)
{
	MemoryContext context = (MemoryContext) key;
	pg_query_free_top_memory_context(context);
}
#endif

void pg_query_exit(void)
{
#ifdef PG_QUERY_HAVE_THREAD_CLEANUP
	pg_query_thread_setspecific(pg_query_thread_exit_key, NULL);
#endif
	pg_query_free_top_memory_context(TopMemoryContext);
}

MemoryContext pg_query_enter_memory_context()
{
	MemoryContext ctx = NULL;

	pg_query_init();

	Assert(CurrentMemoryContext == TopMemoryContext);
	ctx = AllocSetContextCreate(TopMemoryContext,
								"pg_query",
								ALLOCSET_DEFAULT_SIZES);
	MemoryContextSwitchTo(ctx);

	return ctx;
}

void pg_query_exit_memory_context(MemoryContext ctx)
{
	// Return to previous PostgreSQL memory context
	MemoryContextSwitchTo(TopMemoryContext);

	MemoryContextDelete(ctx);
	ctx = NULL;
}

void pg_query_free_error(PgQueryError *error)
{
	free(error->message);
	free(error->funcname);
	free(error->filename);

	if (error->context) {
		free(error->context);
	}

	free(error);
}
