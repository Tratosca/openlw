/*
 * Couche C du daemon OpenLW (macOS) : threads temps réel, os_log, contrôle XPC.
 * Interface volontairement minimale : chaînes C UTF-8 et pointeurs opaques.
 */
#ifndef LW_SYS_H
#define LW_SYS_H

#include <stddef.h>
#include <stdint.h>

/* Passe le thread appelant en THREAD_TIME_CONSTRAINT_POLICY. Retourne un kern_return_t (0 = succès). */
int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns);

/* Sommeil précis de ns nanosecondes (mach_wait_until) : à utiliser dans un thread temps réel,
 * qui ne doit jamais attendre activement (dépassement du budget de calcul → rétrogradation). */
void lw_sleep_ns(uint64_t ns);

/* Journal unifié, sous-système "fr.francois-brille.openlw", catégorie donnée.
 * level : 0 debug, 1 info, 2 défaut, 3 erreur, 4 faute. */
void lw_log(int level, const char *category, const char *message);

/* Contrôle XPC : requête et réponse JSON dans la clé "json" d'un dictionnaire. */
/* peer_uid : UID effectif du processus appelant (xpc_connection_get_euid), pour l'autorisation. */
typedef char *(*lw_handler_fn)(const char *request, uint32_t peer_uid, void *ctx);
typedef void (*lw_free_fn)(char *response);

typedef struct lw_server lw_server;
typedef struct lw_client lw_client;

/* mach_name == NULL : écouteur anonyme (tests, même processus). */
lw_server *lw_xpc_server_start(const char *mach_name, lw_handler_fn handler, lw_free_fn free_response, void *ctx);
/* Point d'accès d'un écouteur (objet XPC retenu, à libérer par lw_xpc_release). */
void *lw_xpc_server_endpoint(lw_server *server);
/* Arrête l'écouteur et attend la fin des traitements en cours ; ctx n'est plus utilisé ensuite. */
void lw_xpc_server_stop(lw_server *server);

lw_client *lw_xpc_client_mach(const char *mach_name, int privileged);
lw_client *lw_xpc_client_endpoint(void *endpoint);
/* Requête synchrone. Retourne une chaîne à libérer par lw_free, ou NULL (erreur décrite dans *error, statique). */
char *lw_xpc_call(lw_client *client, const char *request, const char **error);
void lw_xpc_client_close(lw_client *client);

/* Région partagée remise en réponse aux requêtes qui la demandent (objet xpc_shmem, retenu). */
void lw_xpc_server_set_shmem(lw_server *server, void *shmem);
/* Comme lw_xpc_call, en demandant la région partagée : *shmem reçoit un objet retenu ou NULL. */
char *lw_xpc_call_shmem(lw_client *client, const char *request, const char **error, void **shmem);

/* Région partagée : allocation (mmap partagé + objet xpc_shmem retenu), mappage d'un objet reçu, démappage. */
void *lw_shm_alloc(size_t size, void **shmem);
void *lw_shm_map(void *shmem, size_t *size);
void lw_shm_unmap(void *base, size_t size);

/* Horloge hôte (mach_absolute_time) et conversion en nanosecondes. */
uint64_t lw_host_time(void);
uint64_t lw_host_time_to_ns(uint64_t t);

/* 1 si l'utilisateur `uid` est root ou membre du groupe `group` (ex. "admin"), 0 sinon. */
int lw_uid_in_group(uint32_t uid, const char *group);

void lw_xpc_release(void *object);
void lw_free(char *p);

#endif
