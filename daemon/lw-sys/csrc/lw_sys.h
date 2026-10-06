/*
 * Couche C du daemon OpenLW : temps réel, horloge hôte, journal, mémoire partagée, contrôle.
 * Interface volontairement minimale : chaînes C UTF-8 et pointeurs opaques. Une implémentation
 * par système : macos/lw_sys_macos.c, linux/lw_sys_linux.c, windows/lw_sys_win.c ; posix/lw_posix.c
 * complète macOS et Linux.
 */
#ifndef LW_SYS_H
#define LW_SYS_H

#include <stddef.h>
#include <stdint.h>

#include "lw_shm.h"

/* ---------- Commun à tous les systèmes ---------- */

/* Passe le thread appelant en temps réel (période, budget de calcul, échéance en ns).
 * macOS : THREAD_TIME_CONSTRAINT_POLICY ; Linux : SCHED_FIFO ; Windows : MMCSS « Pro Audio ».
 * Retourne 0 si succès, un code d'erreur du système sinon. */
int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns);

/* Sommeil précis de ns nanosecondes, sans attente active : seul mode d'attente admissible dans un
 * thread temps réel. */
void lw_sleep_ns(uint64_t ns);

/* Journal du système. level : 0 debug, 1 info, 2 défaut, 3 erreur, 4 faute.
 * macOS : os_log (sous-système fr.francois-brille.openlw) ; Windows : débogueur et journal des
 * événements (source OpenLW) ; Linux : sans effet (stderr, recueilli par journald, suffit). */
void lw_log(int level, const char *category, const char *message);

/* Horloge hôte brute (unité de clock_host_time), conversion en nanosecondes, description. */
uint64_t lw_host_time(void);
uint64_t lw_host_time_to_ns(uint64_t t);
void lw_host_clock_info(lw_host_clock *clock);

/* Région partagée : allocation de `size` octets mis à zéro. *handle reçoit l'objet de partage
 * (macOS : xpc_shmem retenu ; Windows : HANDLE de section ; Linux : NULL, région locale au processus). */
void *lw_shm_alloc(size_t size, void **handle);
/* Mappe un objet de partage reçu ; *size reçoit la taille mappée. NULL si échec. */
void *lw_shm_map(void *handle, size_t *size);
void lw_shm_unmap(void *base, size_t size);
/* Libère l'objet de partage (sans effet si NULL). */
void lw_shm_release(void *handle);

void lw_free(char *p);

/* ---------- macOS et Linux ---------- */
#if !defined(_WIN32)

/* 1 si l'utilisateur `uid` est root ou membre du groupe `group` (ex. "admin"), 0 sinon. */
int lw_uid_in_group(uint32_t uid, const char *group);
/* Identité du processus à l'autre bout d'une socket Unix connectée. Retourne 0 si succès. */
int lw_peer_cred(int fd, uint32_t *uid, uint32_t *pid);
/* UID effectif du processus courant. */
uint32_t lw_geteuid(void);

#endif

/* ---------- macOS : contrôle XPC ---------- */
#if defined(__APPLE__)

/* Requête et réponse JSON dans la clé "json" d'un dictionnaire.
 * peer_uid : UID effectif du processus appelant (xpc_connection_get_euid), pour l'autorisation. */
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

void lw_xpc_release(void *object);

#endif

/* ---------- Windows : contrôle par tube nommé ---------- */
#if defined(_WIN32)

/* Appelant d'un tube nommé, identifié par le jeton du client (niveau Identification). */
typedef struct {
    uint32_t pid;
    int may_edit;    /* administrateur (session élevée), LocalSystem, ou membre du groupe d'édition */
    char user[256];  /* DOMAINE\utilisateur, UTF-8 */
} lw_caller;

typedef char *(*lw_pipe_handler_fn)(const char *request, const lw_caller *caller, void *ctx);
typedef void (*lw_pipe_free_fn)(char *response);

typedef struct lw_pipe_server lw_pipe_server;
typedef struct lw_pipe_client lw_pipe_client;

/* Écoute \\.\pipe\<name> : une requête JSON par ligne, une réponse par ligne. `edit_group` : groupe
 * local dont les membres peuvent modifier la configuration (NULL : administrateurs seulement).
 * Le premier exemplaire du tube est créé avec FILE_FLAG_FIRST_PIPE_INSTANCE : échec si un autre
 * processus occupe déjà le nom. */
lw_pipe_server *lw_pipe_server_start(const char *name, const char *edit_group, lw_pipe_handler_fn handler,
                                     lw_pipe_free_fn free_response, void *ctx);
/* Arrête l'écoute, ferme les connexions et attend la fin des traitements en cours. */
void lw_pipe_server_stop(lw_pipe_server *server);

/* Connexion (attente jusqu'à timeout_ms si toutes les instances sont occupées). */
lw_pipe_client *lw_pipe_client_connect(const char *name, uint32_t timeout_ms, const char **error);
/* Requête synchrone : `request` ne doit pas contenir de saut de ligne. Retourne une chaîne à libérer
 * par lw_free, ou NULL (erreur décrite dans *error, statique). */
char *lw_pipe_call(lw_pipe_client *client, const char *request, const char **error);
void lw_pipe_client_close(lw_pipe_client *client);

/* Duplique la section `handle` dans le processus `pid` (lecture et écriture). Retourne la valeur du
 * handle dans ce processus, 0 si échec. */
uint64_t lw_shm_share_with(void *handle, uint32_t pid);

/* Marque DSCP `dscp` (0 à 63) sur les envois de la socket `sock` vers dest_ip:dest_port (qWAVE).
 * Retourne un flux à fermer par lw_qos_close avant la socket, ou NULL (*error : code Windows). */
void *lw_qos_dscp(uint64_t sock, const uint8_t dest_ip[4], uint16_t dest_port, uint32_t dscp, int *error);
void lw_qos_close(void *flow);

#endif

#endif
