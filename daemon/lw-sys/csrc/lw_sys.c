/*
 * Couche C du daemon OpenLW (macOS). Compilée sans ARC : gestion explicite xpc_retain/xpc_release.
 * Disponibilité : thread_policy_set (10.0), os_log (10.12), XPC C API (10.7) — compatible plancher 10.13.
 */
#include "lw_sys.h"

#include <dispatch/dispatch.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <mach/thread_policy.h>
#include <os/log.h>
#include <stdlib.h>
#include <string.h>
#include <grp.h>
#include <pwd.h>
#include <sys/mman.h>
#include <unistd.h>
#include <xpc/xpc.h>

/* ---------- Threads temps réel ---------- */

static uint32_t ns_to_abs(uint64_t ns) {
    static mach_timebase_info_data_t tb;
    if (tb.denom == 0) {
        mach_timebase_info(&tb);
    }
    uint64_t abs = ns * tb.denom / tb.numer;
    return abs > UINT32_MAX ? UINT32_MAX : (uint32_t)abs;
}

int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns) {
    thread_time_constraint_policy_data_t p;
    p.period = ns_to_abs(period_ns);
    p.computation = ns_to_abs(computation_ns);
    p.constraint = ns_to_abs(constraint_ns);
    p.preemptible = 1;
    mach_port_t thread = mach_thread_self();
    kern_return_t kr = thread_policy_set(thread, THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&p,
                                         THREAD_TIME_CONSTRAINT_POLICY_COUNT);
    mach_port_deallocate(mach_task_self(), thread);
    return kr;
}

void lw_sleep_ns(uint64_t ns) {
    static mach_timebase_info_data_t tb;
    if (tb.denom == 0) {
        mach_timebase_info(&tb);
    }
    mach_wait_until(mach_absolute_time() + ns * tb.denom / tb.numer);
}

/* ---------- Journal unifié ---------- */

void lw_log(int level, const char *category, const char *message) {
    /* os_log_create mémorise les objets par (sous-système, catégorie) : appel répété peu coûteux. */
    os_log_t log = os_log_create("fr.francois-brille.openlw", category ? category : "daemon");
    os_log_type_t type = level <= 0   ? OS_LOG_TYPE_DEBUG
                         : level == 1 ? OS_LOG_TYPE_INFO
                         : level == 2 ? OS_LOG_TYPE_DEFAULT
                         : level == 3 ? OS_LOG_TYPE_ERROR
                                      : OS_LOG_TYPE_FAULT;
    os_log_with_type(log, type, "%{public}s", message ? message : "");
}

/* ---------- Contrôle XPC ---------- */

struct lw_server {
    xpc_connection_t listener;
    dispatch_queue_t queue;
    lw_handler_fn handler;
    lw_free_fn free_response;
    void *ctx;
    xpc_object_t shmem; /* région partagée remise aux clients qui la demandent (want_shmem) */
};

struct lw_client {
    xpc_connection_t conn;
};

static void handle_peer(lw_server *s, xpc_connection_t peer) {
    xpc_retain(peer);
    xpc_connection_set_target_queue(peer, s->queue);
    xpc_connection_set_event_handler(peer, ^(xpc_object_t ev) {
      if (xpc_get_type(ev) == XPC_TYPE_ERROR) {
          if (ev == XPC_ERROR_CONNECTION_INVALID) {
              xpc_release(peer);
          }
          return;
      }
      if (xpc_get_type(ev) != XPC_TYPE_DICTIONARY) {
          return;
      }
      xpc_object_t reply = xpc_dictionary_create_reply(ev);
      if (reply == NULL) {
          return;
      }
      const char *req = xpc_dictionary_get_string(ev, "json");
      char *resp = s->handler(req ? req : "", (uint32_t)xpc_connection_get_euid(peer), s->ctx);
      xpc_dictionary_set_string(reply, "json", resp ? resp : "{\"ok\":false,\"error\":\"réponse vide\"}");
      if (s->shmem != NULL && xpc_dictionary_get_bool(ev, "want_shmem")) {
          xpc_dictionary_set_value(reply, "shmem", s->shmem);
      }
      if (resp) {
          s->free_response(resp);
      }
      xpc_connection_send_message(peer, reply);
      xpc_release(reply);
    });
    xpc_connection_resume(peer);
}

lw_server *lw_xpc_server_start(const char *mach_name, lw_handler_fn handler, lw_free_fn free_response, void *ctx) {
    lw_server *s = calloc(1, sizeof *s);
    if (s == NULL) {
        return NULL;
    }
    s->handler = handler;
    s->free_response = free_response;
    s->ctx = ctx;
    s->queue = dispatch_queue_create("fr.francois-brille.openlw.xpc", DISPATCH_QUEUE_SERIAL);
    s->listener = mach_name ? xpc_connection_create_mach_service(mach_name, s->queue, XPC_CONNECTION_MACH_SERVICE_LISTENER)
                            : xpc_connection_create(NULL, s->queue);
    if (s->listener == NULL) {
        dispatch_release(s->queue);
        free(s);
        return NULL;
    }
    xpc_connection_set_event_handler(s->listener, ^(xpc_object_t ev) {
      if (xpc_get_type(ev) == XPC_TYPE_CONNECTION) {
          handle_peer(s, (xpc_connection_t)ev);
      }
    });
    xpc_connection_resume(s->listener);
    return s;
}

void lw_xpc_server_set_shmem(lw_server *s, void *shmem) {
    if (s == NULL) {
        return;
    }
    xpc_object_t obj = shmem ? xpc_retain((xpc_object_t)shmem) : NULL;
    dispatch_sync(s->queue, ^{
      if (s->shmem != NULL) {
          xpc_release(s->shmem);
      }
      s->shmem = obj;
    });
}

void *lw_xpc_server_endpoint(lw_server *s) {
    return s ? (void *)xpc_endpoint_create(s->listener) : NULL;
}

void lw_xpc_server_stop(lw_server *s) {
    if (s == NULL) {
        return;
    }
    xpc_connection_cancel(s->listener);
    /* Barrière : aucun gestionnaire ne s'exécute plus après ce point. */
    dispatch_sync(s->queue, ^{
                  });
    s->handler = NULL;
    if (s->shmem != NULL) {
        xpc_release(s->shmem);
    }
    xpc_release(s->listener);
    dispatch_release(s->queue);
    free(s);
}

static lw_client *client_from(xpc_connection_t conn) {
    if (conn == NULL) {
        return NULL;
    }
    xpc_connection_set_event_handler(conn, ^(xpc_object_t ev) {
      (void)ev; /* erreurs remontées par l'appel synchrone */
    });
    xpc_connection_resume(conn);
    lw_client *c = calloc(1, sizeof *c);
    if (c == NULL) {
        xpc_connection_cancel(conn);
        xpc_release(conn);
        return NULL;
    }
    c->conn = conn;
    return c;
}

lw_client *lw_xpc_client_mach(const char *mach_name, int privileged) {
    return client_from(
        xpc_connection_create_mach_service(mach_name, NULL, privileged ? XPC_CONNECTION_MACH_SERVICE_PRIVILEGED : 0));
}

lw_client *lw_xpc_client_endpoint(void *endpoint) {
    return endpoint ? client_from(xpc_connection_create_from_endpoint((xpc_endpoint_t)endpoint)) : NULL;
}

char *lw_xpc_call(lw_client *c, const char *request, const char **error) {
    return lw_xpc_call_shmem(c, request, error, NULL);
}

char *lw_xpc_call_shmem(lw_client *c, const char *request, const char **error, void **shmem) {
    *error = NULL;
    if (shmem) {
        *shmem = NULL;
    }
    xpc_object_t msg = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_string(msg, "json", request ? request : "");
    if (shmem) {
        xpc_dictionary_set_bool(msg, "want_shmem", true);
    }
    xpc_object_t reply = xpc_connection_send_message_with_reply_sync(c->conn, msg);
    xpc_release(msg);
    char *out = NULL;
    if (xpc_get_type(reply) == XPC_TYPE_DICTIONARY) {
        const char *json = xpc_dictionary_get_string(reply, "json");
        if (json) {
            out = strdup(json);
            xpc_object_t obj = shmem ? xpc_dictionary_get_value(reply, "shmem") : NULL;
            if (obj != NULL && xpc_get_type(obj) == XPC_TYPE_SHMEM) {
                *shmem = xpc_retain(obj);
            }
        } else {
            *error = "réponse XPC sans clé json";
        }
    } else if (reply == XPC_ERROR_CONNECTION_INVALID) {
        *error = "service XPC introuvable ou connexion refusée";
    } else if (reply == XPC_ERROR_CONNECTION_INTERRUPTED) {
        *error = "connexion XPC interrompue";
    } else {
        *error = "erreur XPC";
    }
    xpc_release(reply);
    return out;
}

void lw_xpc_client_close(lw_client *c) {
    if (c == NULL) {
        return;
    }
    xpc_connection_cancel(c->conn);
    xpc_release(c->conn);
    free(c);
}

void lw_xpc_release(void *object) {
    if (object) {
        xpc_release((xpc_object_t)object);
    }
}

void lw_free(char *p) {
    free(p);
}

/* ---------- Région partagée : création, transfert, mappage ---------- */

void *lw_shm_alloc(size_t size, void **shmem) {
    *shmem = NULL;
    void *base = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_ANON | MAP_SHARED, -1, 0);
    if (base == MAP_FAILED) {
        return NULL;
    }
    xpc_object_t obj = xpc_shmem_create(base, size);
    if (obj == NULL) {
        munmap(base, size);
        return NULL;
    }
    *shmem = obj;
    return base;
}

void *lw_shm_map(void *shmem, size_t *size) {
    void *base = NULL;
    *size = shmem ? xpc_shmem_map((xpc_object_t)shmem, &base) : 0;
    return *size ? base : NULL;
}

void lw_shm_unmap(void *base, size_t size) {
    if (base != NULL && size != 0) {
        munmap(base, size);
    }
}

uint64_t lw_host_time(void) {
    return mach_absolute_time();
}

uint64_t lw_host_time_to_ns(uint64_t t) {
    static mach_timebase_info_data_t tb;
    if (tb.denom == 0) {
        mach_timebase_info(&tb);
    }
    return t * tb.numer / tb.denom;
}

/* ---------- Autorisation ---------- */

int lw_uid_in_group(uint32_t uid, const char *group) {
    if (uid == 0) {
        return 1;
    }
    if (group == NULL) {
        return 0;
    }
    struct passwd pw, *pres = NULL;
    char pwbuf[4096];
    if (getpwuid_r((uid_t)uid, &pw, pwbuf, sizeof pwbuf, &pres) != 0 || pres == NULL) {
        return 0;
    }
    struct group gr, *gres = NULL;
    char grbuf[16384];
    if (getgrnam_r(group, &gr, grbuf, sizeof grbuf, &gres) != 0 || gres == NULL) {
        return 0;
    }
    if (pw.pw_gid == gr.gr_gid) {
        return 1;
    }
    int groups[256];
    int n = 256;
    if (getgrouplist(pw.pw_name, (int)pw.pw_gid, groups, &n) == -1) {
        n = 256;
    }
    for (int i = 0; i < n && i < 256; i++) {
        if ((gid_t)groups[i] == gr.gr_gid) {
            return 1;
        }
    }
    return 0;
}
