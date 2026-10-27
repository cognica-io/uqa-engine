/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef PG_QUERY_THREAD_H
#define PG_QUERY_THREAD_H

/* Keep cleanup owners independent of compiler TLS destruction order. */
#if defined(_WIN32) || defined(_WIN64)
#include <windows.h>

#define PG_QUERY_HAVE_THREAD_CLEANUP 1
#define PG_QUERY_THREAD_CALLBACK CALLBACK
#define PG_QUERY_THREAD_ONCE_INIT INIT_ONCE_STATIC_INIT
typedef DWORD PgQueryThreadKey;
typedef INIT_ONCE PgQueryThreadOnce;

typedef struct PgQueryThreadInit
{
	void (*run)(void);
} PgQueryThreadInit;

static BOOL CALLBACK
pg_query_thread_initialize(PINIT_ONCE once, PVOID parameter, PVOID *context)
{
	PgQueryThreadInit *init = parameter;
	(void) once;
	(void) context;
	init->run();
	return TRUE;
}

static inline int
pg_query_thread_once(PgQueryThreadOnce *once, void (*run)(void))
{
	PgQueryThreadInit init = {run};
	return InitOnceExecuteOnce(once, pg_query_thread_initialize, &init, NULL) ? 0 : 1;
}

static inline int
pg_query_thread_key_create(PgQueryThreadKey *key, void (CALLBACK *destroy)(void *))
{
	*key = FlsAlloc(destroy);
	return *key == FLS_OUT_OF_INDEXES ? 1 : 0;
}

static inline void *
pg_query_thread_getspecific(PgQueryThreadKey key)
{
	return FlsGetValue(key);
}

static inline int
pg_query_thread_setspecific(PgQueryThreadKey key, void *value)
{
	return FlsSetValue(key, value) ? 0 : 1;
}

#elif defined(HAVE_PTHREAD)
#include <pthread.h>

#define PG_QUERY_HAVE_THREAD_CLEANUP 1
#define PG_QUERY_THREAD_CALLBACK
#define PG_QUERY_THREAD_ONCE_INIT PTHREAD_ONCE_INIT
typedef pthread_key_t PgQueryThreadKey;
typedef pthread_once_t PgQueryThreadOnce;
#define pg_query_thread_once pthread_once
#define pg_query_thread_key_create pthread_key_create
#define pg_query_thread_getspecific pthread_getspecific
#define pg_query_thread_setspecific pthread_setspecific

#endif
#endif
