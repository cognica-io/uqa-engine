/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_TEST_THREADS_H
#define UQA_TEST_THREADS_H

/* Native synchronization keeps the same callback-retirement scenario on every ABI. */
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
typedef SRWLOCK test_mutex;
typedef CONDITION_VARIABLE test_condition;
typedef struct test_thread {
    HANDLE handle;
    void *(*run)(void *);
    void *data;
} test_thread;
static int test_mutex_init(test_mutex *mutex) { InitializeSRWLock(mutex); return 0; }
static int test_mutex_lock(test_mutex *mutex) { AcquireSRWLockExclusive(mutex); return 0; }
static int test_mutex_unlock(test_mutex *mutex) { ReleaseSRWLockExclusive(mutex); return 0; }
static int test_mutex_destroy(test_mutex *mutex) { (void)mutex; return 0; }
static int test_condition_init(test_condition *condition) { InitializeConditionVariable(condition); return 0; }
static int test_condition_broadcast(test_condition *condition) { WakeAllConditionVariable(condition); return 0; }
static int test_condition_wait(test_condition *condition, test_mutex *mutex) {
    return SleepConditionVariableSRW(condition, mutex, INFINITE, 0) ? 0 : -1;
}
static int test_condition_destroy(test_condition *condition) { (void)condition; return 0; }
static DWORD WINAPI test_thread_run(LPVOID pointer) {
    test_thread *thread = (test_thread *)pointer;
    thread->run(thread->data);
    return 0;
}
static int test_thread_create(test_thread *thread, void *(*run)(void *), void *data) {
    thread->run = run;
    thread->data = data;
    thread->handle = CreateThread(NULL, 0, test_thread_run, thread, 0, NULL);
    return thread->handle == NULL ? -1 : 0;
}
static int test_thread_join(test_thread *thread) {
    if (WaitForSingleObject(thread->handle, INFINITE) != WAIT_OBJECT_0) return -1;
    return CloseHandle(thread->handle) ? 0 : -1;
}
#else
#include <pthread.h>
typedef pthread_mutex_t test_mutex;
typedef pthread_cond_t test_condition;
typedef pthread_t test_thread;
static int test_mutex_init(test_mutex *mutex) { return pthread_mutex_init(mutex, NULL); }
static int test_mutex_lock(test_mutex *mutex) { return pthread_mutex_lock(mutex); }
static int test_mutex_unlock(test_mutex *mutex) { return pthread_mutex_unlock(mutex); }
static int test_mutex_destroy(test_mutex *mutex) { return pthread_mutex_destroy(mutex); }
static int test_condition_init(test_condition *condition) { return pthread_cond_init(condition, NULL); }
static int test_condition_broadcast(test_condition *condition) { return pthread_cond_broadcast(condition); }
static int test_condition_wait(test_condition *condition, test_mutex *mutex) { return pthread_cond_wait(condition, mutex); }
static int test_condition_destroy(test_condition *condition) { return pthread_cond_destroy(condition); }
static int test_thread_create(test_thread *thread, void *(*run)(void *), void *data) {
    return pthread_create(thread, NULL, run, data);
}
static int test_thread_join(test_thread *thread) { return pthread_join(*thread, NULL); }
#endif

#endif /* UQA_TEST_THREADS_H */
