/*
 * OpenLW daemon C layer: real-time scheduling, host clock, logging, shared memory, control.
 * Deliberately minimal interface: UTF-8 C strings and opaque pointers. One implementation
 * per system: macos/lw_sys_macos.c, linux/lw_sys_linux.c, windows/lw_sys_win.c; posix/lw_posix.c
 * complements macOS and Linux.
 */
#ifndef LW_SYS_H
#define LW_SYS_H

#include <stddef.h>
#include <stdint.h>

#include "lw_shm.h"

/* ---------- Common to all systems ---------- */

/* Promote calling thread to real-time (period, compute budget, deadline in ns).
 * macOS: THREAD_TIME_CONSTRAINT_POLICY; Linux: SCHED_FIFO; Windows: MMCSS “Pro Audio”.
 * Return zero on success, otherwise a system error code. */
int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns);

/* Precise sleep for ns nanoseconds, without busy-waiting: the only acceptable waiting mode in a
 * real-time thread. */
void lw_sleep_ns(uint64_t ns);

/* System logging. level: 0 debug, 1 info, 2 default, 3 error, 4 fault.
 * macOS: os_log (subsystem fr.francois-brille.openlw); Windows: debugger and Event Log
 * (OpenLW source); Linux: no effect (stderr collected by journald suffices). */
void lw_log(int level, const char *category, const char *message);

/* Raw host clock (clock_host_time unit), nanosecond conversion, description. */
uint64_t lw_host_time(void);
uint64_t lw_host_time_to_ns(uint64_t t);
void lw_host_clock_info(lw_host_clock *clock);

/* Shared region: allocate `size` zeroed bytes. *handle receives the sharing object
 * (macOS: retained xpc_shmem; Windows: section HANDLE; Linux: NULL, process-local region). */
void *lw_shm_alloc(size_t size, void **handle);
/* Map a received sharing object; *size receives mapped size. NULL on failure. */
void *lw_shm_map(void *handle, size_t *size);
void lw_shm_unmap(void *base, size_t size);
/* Release sharing object (no effect if NULL). */
void lw_shm_release(void *handle);

void lw_free(char *p);

/* ---------- macOS and Linux ---------- */
#if !defined(_WIN32)

/* 1 if user `uid` is root or belongs to `group` (e.g. "admin"), otherwise 0. */
int lw_uid_in_group(uint32_t uid, const char *group);
/* Process identity at the other end of a connected Unix socket. Return zero on success. */
int lw_peer_cred(int fd, uint32_t *uid, uint32_t *pid);
/* Current process effective UID. */
uint32_t lw_geteuid(void);

#endif

/* ---------- macOS: XPC control ---------- */
#if defined(__APPLE__)

/* JSON request/response in a dictionary's "json" key.
 * peer_uid: calling process effective UID (xpc_connection_get_euid), for authorization. */
typedef char *(*lw_handler_fn)(const char *request, uint32_t peer_uid, void *ctx);
typedef void (*lw_free_fn)(char *response);

typedef struct lw_server lw_server;
typedef struct lw_client lw_client;

/* mach_name == NULL: anonymous listener (tests, same process). */
lw_server *lw_xpc_server_start(const char *mach_name, lw_handler_fn handler, lw_free_fn free_response, void *ctx);
/* Listener endpoint (retained XPC object, release with lw_xpc_release). */
void *lw_xpc_server_endpoint(lw_server *server);
/* Stop listener and wait for active handlers; ctx is no longer used afterwards. */
void lw_xpc_server_stop(lw_server *server);

lw_client *lw_xpc_client_mach(const char *mach_name, int privileged);
lw_client *lw_xpc_client_endpoint(void *endpoint);
/* Synchronous request. Return a string to free with lw_free, or NULL (static error in *error). */
char *lw_xpc_call(lw_client *client, const char *request, const char **error);
void lw_xpc_client_close(lw_client *client);

/* Shared region returned to requesting clients (retained xpc_shmem object). */
void lw_xpc_server_set_shmem(lw_server *server, void *shmem);
/* Like lw_xpc_call, requesting shared region: *shmem receives a retained object or NULL. */
char *lw_xpc_call_shmem(lw_client *client, const char *request, const char **error, void **shmem);

void lw_xpc_release(void *object);

#endif

/* ---------- Windows: named-pipe control ---------- */
#if defined(_WIN32)

/* Named-pipe caller identified by client token (Identification level). */
typedef struct {
    uint32_t pid;
    int may_edit;    /* Elevated administrator, LocalSystem, or edit-group member */
    char user[256];  /* DOMAIN\user, UTF-8 */
} lw_caller;

typedef char *(*lw_pipe_handler_fn)(const char *request, const lw_caller *caller, void *ctx);
typedef void (*lw_pipe_free_fn)(char *response);

typedef struct lw_pipe_server lw_pipe_server;
typedef struct lw_pipe_client lw_pipe_client;

/* Listen on \\.\pipe\<name>: one JSON request and response per line. `edit_group`: local
 * group whose members may modify configuration (NULL: administrators only).
 * First pipe instance uses FILE_FLAG_FIRST_PIPE_INSTANCE: fail if another
 * process already occupies the name. */
lw_pipe_server *lw_pipe_server_start(const char *name, const char *edit_group, lw_pipe_handler_fn handler,
                                     lw_pipe_free_fn free_response, void *ctx);
/* Stop listening, close connections, and wait for active handlers. */
void lw_pipe_server_stop(lw_pipe_server *server);

/* Connect (wait up to timeout_ms if all instances are busy). */
lw_pipe_client *lw_pipe_client_connect(const char *name, uint32_t timeout_ms, const char **error);
/* Synchronous request: `request` must contain no newline. Return string to free
 * with lw_free, or NULL (static error described in *error). */
char *lw_pipe_call(lw_pipe_client *client, const char *request, const char **error);
void lw_pipe_client_close(lw_pipe_client *client);

/* Duplicate section `handle` into process `pid` (read/write). Return handle value
 * in that process, zero on failure. */
uint64_t lw_shm_share_with(void *handle, uint32_t pid);

/* Mark DSCP `dscp` (0–63) on socket `sock` sends to dest_ip:dest_port (qWAVE).
 * Return flow to close with lw_qos_close before the socket, or NULL (*error: Windows code). */
void *lw_qos_dscp(uint64_t sock, const uint8_t dest_ip[4], uint16_t dest_port, uint32_t dscp, int *error);
void lw_qos_close(void *flow);

/* 1 if process `pid` is alive (UTF-8 image name in name[cap]), otherwise 0. */
int lw_process_alive(uint32_t pid, char *name, size_t cap);

#endif

#endif
