/*
 * Daemon ↔ audio client shared region (ADR 0005). Single contract: HAL plugin (macOS),
 * Windows driver, and daemon (Rust through FFI) compile this file and lw_shm.c. No other
 * layout definition.
 *
 * Layout: [8192-byte header][ring 0][ring 1]…, interleaved float32 samples. The header
 * holds a ring table: each ring has a direction and its own channel count; all rings share
 * ring_frames and the clock.
 *   TO_NET: application audio → network. Producer: audio client; consumer: daemon.
 *   FROM_NET: network audio → applications. Producer: daemon; consumer: audio client.
 * Order: TO_NET rings first, then FROM_NET rings. A duplex region (one device) has two
 * rings, so ring indices LW_TO_NET (0) and LW_FROM_NET (1). A multi-device region (macOS)
 * has one ring per device: Out 1..M, then In 1..N.
 * Each ring is SPSC (one producer, one consumer), with 64-bit frame positions,
 * never reset; index = position & (ring_frames - 1).
 *
 * Real-time rule: no function blocks, allocates, or calls the system (usable in
 * CoreAudio I/O thread or Windows driver audio callback).
 *
 * Compilers: Clang/GCC (C11), MSVC (/std:c11 or C++).
 */
#ifndef LW_SHM_H
#define LW_SHM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LW_SHM_MAGIC 0x4C57534Du /* "LWSM" */
#define LW_SHM_VERSION 3u
#define LW_SHM_HEADER_BYTES 8192u
#define LW_SHM_MAX_CHANNELS 64u /* Per ring */
#define LW_SHM_MAX_RINGS 32u
#define LW_SHM_MAX_RING_FRAMES 65536u

/* Ring direction; also the ring index in a duplex region. */
enum lw_dir { LW_TO_NET = 0, LW_FROM_NET = 1 };

/* Ring description: direction (enum lw_dir) and channel count (interleaved frame width). */
typedef struct {
    uint32_t dir;
    uint32_t channels;
} lw_ring_spec;

/* Host clock in which clock_host_time is expressed. */
enum lw_host_clock_id {
    LW_CLOCK_UNKNOWN = 0,
    LW_CLOCK_MACH = 1,      /* macOS: mach_absolute_time */
    LW_CLOCK_QPC = 2,       /* Windows: QueryPerformanceCounter */
    LW_CLOCK_MONOTONIC = 3, /* Linux: CLOCK_MONOTONIC in nanoseconds */
};

/* Host clock description: duration in ns = ticks × ns_numer / ns_denom. */
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

/* Cache-line padding below is intended (MSVC warning C4324). */
#if defined(_MSC_VER) && !defined(__clang__)
#pragma warning(push)
#pragma warning(disable : 4324)
#endif

/* Ring counters/positions; producer and consumer on separate cache lines. */
typedef struct {
    LW_ALIGN64 uint64_t write_pos; /* Written by producer (release) */
    uint64_t overruns;             /* Frames rejected due to insufficient space (producer) */
    LW_ALIGN64 uint64_t read_pos;  /* Written by consumer (release) */
    uint64_t underruns;            /* Missing frames padded with silence (consumer) */
} lw_ring_pos;

typedef struct {
    uint32_t magic;
    uint32_t version;
    uint32_t header_bytes;
    uint32_t sample_rate;
    uint32_t ring_frames;
    uint32_t ring_count;
    uint32_t host_clock_id;
    uint32_t reserved;
    uint64_t total_bytes;
    uint64_t host_ns_numer;
    uint64_t host_ns_denom;
    /* Daemon-published clock (seqlock): at host instant host_time (clock host_clock_id),
     * sample position is sample_time; rate_scalar = actual sample duration /
     * nominal duration (1.0 if network clock = host clock). */
    LW_ALIGN64 uint32_t clock_seq; /* Odd while writing */
    uint32_t clock_valid;
    uint64_t clock_host_time;
    uint64_t clock_sample_time;
    double clock_rate_scalar;
    lw_ring_spec spec[LW_SHM_MAX_RINGS];
    uint64_t offset[LW_SHM_MAX_RINGS]; /* Ring data offset from region start */
    lw_ring_pos ring[LW_SHM_MAX_RINGS];
} lw_shm_header;

#if defined(_MSC_VER) && !defined(__clang__)
#pragma warning(pop)
#endif

#ifdef __cplusplus
static_assert(sizeof(lw_shm_header) <= LW_SHM_HEADER_BYTES, "header too large");
#else
_Static_assert(sizeof(lw_shm_header) <= LW_SHM_HEADER_BYTES, "header too large");
#endif

/* Total region size; zero if parameters are invalid (ring_frames power of two in
 * [64, LW_SHM_MAX_RING_FRAMES], 1 to LW_SHM_MAX_RINGS rings, TO_NET rings before FROM_NET
 * rings, channels in [0, LW_SHM_MAX_CHANNELS]). */
size_t lw_shm_size(uint32_t ring_frames, uint32_t ring_count, const lw_ring_spec *specs);

/* Initialize lw_shm_size(...) bytes including zeroing. `clock` describes the clock
 * for clock_host_time (NULL: unknown). Return zero on success. */
int lw_shm_init(void *base, size_t size, uint32_t sample_rate, uint32_t ring_frames, uint32_t ring_count,
                const lw_ring_spec *specs, const lw_host_clock *clock);

/* Validate a received region (magic, version, sizes consistent with `size`). Return zero if valid. */
int lw_shm_validate(const void *base, size_t size);

/* Host clock declared by the region creator. */
void lw_shm_host_clock(const void *base, lw_host_clock *clock);

/* Geometry of a validated region. lw_ring_dir and lw_ring_channels return zero for an
 * out-of-range ring (lw_ring_dir: also check ring < lw_shm_ring_count). */
uint32_t lw_shm_sample_rate(const void *base);
uint32_t lw_shm_ring_frames(const void *base);
uint32_t lw_shm_ring_count(const void *base);
uint32_t lw_ring_dir(const void *base, uint32_t ring);
uint32_t lw_ring_channels(const void *base, uint32_t ring);

/* Ring functions take a ring index; out-of-range rings are inert (zero, nothing written).
 * Write up to `frames` interleaved frames (ring channel count). Return count written;
 * count remainder as overruns. Reserved for ring producer. */
uint32_t lw_ring_write(void *base, uint32_t ring, const float *src, uint32_t frames);

/* Read exactly `frames` frames into dst; pad missing frames with silence and count
 * underruns. Return actual frame count read. Reserved for consumer. */
uint32_t lw_ring_read(void *base, uint32_t ring, float *dst, uint32_t frames);

/* Discard up to `frames` oldest frames (latency catch-up). Return discarded
 * count. Reserved for consumer. */
uint32_t lw_ring_skip(void *base, uint32_t ring, uint32_t frames);

/* Readable frames (consumer side) and free space (producer side). */
uint32_t lw_ring_readable(const void *base, uint32_t ring);
uint32_t lw_ring_writable(const void *base, uint32_t ring);

/* Ring counters. */
void lw_ring_counters(const void *base, uint32_t ring, uint64_t *write_pos, uint64_t *read_pos, uint64_t *overruns,
                      uint64_t *underruns);

/* Clock: publication (daemon, single writer) and coherent reading (client). lw_clock_read
 * returns zero if a valid value was read, -1 if no clock is published yet. */
void lw_clock_publish(void *base, uint64_t host_time, uint64_t sample_time, double rate_scalar);
int lw_clock_read(const void *base, uint64_t *host_time, uint64_t *sample_time, double *rate_scalar);

#ifdef __cplusplus
}
#endif

#endif
