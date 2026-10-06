/*
 * OpenLW daemon C layer, Linux. Daemon runs as a user systemd service and serves
 * PipeWire nodes itself: the shared region remains process-local.
 */
#define _GNU_SOURCE

#include "../lw_sys.h"

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>

/* ---------- Real-time ---------- */

/* Modest SCHED_FIFO priority below PipeWire threads (default 88): network thread
 * must not preempt the audio graph. Requires CAP_SYS_NICE or sufficient RLIMIT_RTPRIO
 * (pipewire/audio group, limits.d); otherwise EPERM and thread remains time-shared. */
#define LW_RT_PRIORITY 70

int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns) {
    (void)period_ns;
    (void)computation_ns;
    (void)constraint_ns;
    struct sched_param sp;
    memset(&sp, 0, sizeof sp);
    sp.sched_priority = LW_RT_PRIORITY;
    int max = sched_get_priority_max(SCHED_FIFO);
    if (max > 0 && sp.sched_priority > max) {
        sp.sched_priority = max;
    }
    return pthread_setschedparam(pthread_self(), SCHED_FIFO, &sp);
}

void lw_sleep_ns(uint64_t ns) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    uint64_t target = (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec + ns;
    t.tv_sec = (time_t)(target / 1000000000ull);
    t.tv_nsec = (long)(target % 1000000000ull);
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &t, NULL) == EINTR) {
    }
}

/* ---------- Logging ---------- */

void lw_log(int level, const char *category, const char *message) {
    (void)level;
    (void)category;
    (void)message;
}

/* ---------- Host clock: CLOCK_MONOTONIC in nanoseconds ---------- */

uint64_t lw_host_time(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

uint64_t lw_host_time_to_ns(uint64_t t) {
    return t;
}

void lw_host_clock_info(lw_host_clock *clock) {
    clock->id = LW_CLOCK_MONOTONIC;
    clock->ns_numer = 1;
    clock->ns_denom = 1;
}

/* ---------- Shared region (process-local) ---------- */

void *lw_shm_alloc(size_t size, void **handle) {
    *handle = NULL;
    void *base = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_SHARED, -1, 0);
    return base == MAP_FAILED ? NULL : base;
}

void *lw_shm_map(void *handle, size_t *size) {
    (void)handle;
    *size = 0;
    return NULL;
}

void lw_shm_unmap(void *base, size_t size) {
    if (base != NULL && size != 0) {
        munmap(base, size);
    }
}

void lw_shm_release(void *handle) {
    (void)handle;
}

void lw_free(char *p) {
    free(p);
}
