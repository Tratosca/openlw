/*
 * Région partagée daemon ↔ client audio (ADR 0005). Contrat unique : le plugin HAL (macOS), le
 * pilote ASIO (Windows) et le daemon (Rust, par FFI) compilent ce fichier et lw_shm.c. Aucune autre
 * définition de la disposition.
 *
 * Disposition : [en-tête 4096 octets][anneau TO_NET][anneau FROM_NET], échantillons float32 entrelacés.
 *   TO_NET   : audio des applications → réseau. Producteur : client audio ; consommateur : daemon.
 *   FROM_NET : audio du réseau → applications. Producteur : daemon ; consommateur : client audio.
 * Chaque anneau est SPSC (un producteur, un consommateur), positions en trames sur 64 bits,
 * jamais remises à zéro ; index = position & (ring_frames - 1).
 *
 * Règle temps réel : aucune fonction ne bloque, n'alloue ni n'appelle le système (utilisable dans
 * le thread IO de CoreAudio ou le callback ASIO).
 *
 * Compilateurs : Clang ou GCC (C11), MSVC (/std:c11 ou C++).
 */
#ifndef LW_SHM_H
#define LW_SHM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LW_SHM_MAGIC 0x4C57534Du /* "LWSM" */
#define LW_SHM_VERSION 2u
#define LW_SHM_HEADER_BYTES 4096u
#define LW_SHM_MAX_CHANNELS 64u
#define LW_SHM_MAX_RING_FRAMES 65536u

enum lw_dir { LW_TO_NET = 0, LW_FROM_NET = 1 };

/* Horloge hôte dans laquelle est exprimé clock_host_time. */
enum lw_host_clock_id {
    LW_CLOCK_UNKNOWN = 0,
    LW_CLOCK_MACH = 1,      /* macOS : mach_absolute_time */
    LW_CLOCK_QPC = 2,       /* Windows : QueryPerformanceCounter */
    LW_CLOCK_MONOTONIC = 3, /* Linux : CLOCK_MONOTONIC en nanosecondes */
};

/* Description de l'horloge hôte : durée en ns = ticks × ns_numer / ns_denom. */
typedef struct {
    uint32_t id; /* enum lw_host_clock_id */
    uint64_t ns_numer;
    uint64_t ns_denom;
} lw_host_clock;

#if defined(_MSC_VER) && !defined(__clang__)
#define LW_ALIGN64 __declspec(align(64))
#elif defined(__cplusplus)
#define LW_ALIGN64 alignas(64)
#else
#define LW_ALIGN64 _Alignas(64)
#endif

/* Compteurs et positions d'un anneau ; producteur et consommateur sur des lignes de cache distinctes. */
typedef struct {
    LW_ALIGN64 uint64_t write_pos; /* écrit par le producteur (release) */
    uint64_t overruns;             /* trames refusées faute de place (producteur) */
    LW_ALIGN64 uint64_t read_pos;  /* écrit par le consommateur (release) */
    uint64_t underruns;            /* trames manquantes complétées par du silence (consommateur) */
} lw_ring_pos;

typedef struct {
    uint32_t magic;
    uint32_t version;
    uint32_t header_bytes;
    uint32_t sample_rate;
    uint32_t ring_frames;
    uint32_t channels[2]; /* [LW_TO_NET], [LW_FROM_NET] */
    uint32_t host_clock_id;
    uint64_t total_bytes;
    uint64_t host_ns_numer;
    uint64_t host_ns_denom;
    /* Horloge publiée par le daemon (seqlock) : à l'instant hôte host_time (horloge host_clock_id),
     * la position d'échantillon vaut sample_time ; rate_scalar = durée réelle d'un échantillon /
     * durée nominale (1.0 si l'horloge réseau = horloge hôte). */
    LW_ALIGN64 uint32_t clock_seq; /* impair pendant l'écriture */
    uint32_t clock_valid;
    uint64_t clock_host_time;
    uint64_t clock_sample_time;
    double clock_rate_scalar;
    lw_ring_pos ring[2];
} lw_shm_header;

#ifdef __cplusplus
static_assert(sizeof(lw_shm_header) <= LW_SHM_HEADER_BYTES, "en-tête trop grand");
#else
_Static_assert(sizeof(lw_shm_header) <= LW_SHM_HEADER_BYTES, "en-tête trop grand");
#endif

/* Taille totale d'une région ; 0 si les paramètres sont invalides
 * (ring_frames puissance de 2 dans [64, LW_SHM_MAX_RING_FRAMES], canaux dans [0, LW_SHM_MAX_CHANNELS]). */
size_t lw_shm_size(uint32_t ring_frames, uint32_t channels_to_net, uint32_t channels_from_net);

/* Initialise une région de lw_shm_size(...) octets (mise à zéro comprise). `clock` décrit l'horloge
 * de clock_host_time (NULL : inconnue). Retourne 0 si succès. */
int lw_shm_init(void *base, size_t size, uint32_t sample_rate, uint32_t ring_frames, uint32_t channels_to_net,
                uint32_t channels_from_net, const lw_host_clock *clock);

/* Vérifie une région reçue (magie, version, tailles cohérentes avec `size`). Retourne 0 si valide. */
int lw_shm_validate(const void *base, size_t size);

/* Horloge hôte déclarée par le créateur de la région. */
void lw_shm_host_clock(const void *base, lw_host_clock *clock);

/* Écrit jusqu'à `frames` trames entrelacées (canaux de l'anneau). Retourne le nombre écrit ;
 * le reste est compté en overruns. Réservé au producteur de l'anneau. */
uint32_t lw_ring_write(void *base, int dir, const float *src, uint32_t frames);

/* Lit exactement `frames` trames dans dst ; s'il en manque, complète par du silence et compte des
 * underruns. Retourne le nombre de trames réellement lues. Réservé au consommateur. */
uint32_t lw_ring_read(void *base, int dir, float *dst, uint32_t frames);

/* Jette jusqu'à `frames` trames parmi les plus anciennes (rattrapage de latence). Retourne le nombre
 * jeté. Réservé au consommateur. */
uint32_t lw_ring_skip(void *base, int dir, uint32_t frames);

/* Trames disponibles à la lecture (côté consommateur) et place libre (côté producteur). */
uint32_t lw_ring_readable(const void *base, int dir);
uint32_t lw_ring_writable(const void *base, int dir);

/* Compteurs d'un anneau. */
void lw_ring_counters(const void *base, int dir, uint64_t *write_pos, uint64_t *read_pos, uint64_t *overruns,
                      uint64_t *underruns);

/* Horloge : publication (daemon, un seul écrivain) et lecture cohérente (client). lw_clock_read
 * retourne 0 si une valeur valide a été lue, -1 si aucune horloge n'est encore publiée. */
void lw_clock_publish(void *base, uint64_t host_time, uint64_t sample_time, double rate_scalar);
int lw_clock_read(const void *base, uint64_t *host_time, uint64_t *sample_time, double *rate_scalar);

#ifdef __cplusplus
}
#endif

#endif
