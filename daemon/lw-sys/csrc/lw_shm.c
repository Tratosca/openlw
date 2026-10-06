/*
 * Implémentation unique de la région partagée (voir lw_shm.h). Atomiques sans verrou, sans appel
 * système : utilisable dans le thread IO du plugin HAL ou le callback ASIO.
 */
#include "lw_shm.h"

#include <string.h>

#define HDR(b) ((lw_shm_header *)(b))
#define CHDR(b) ((const lw_shm_header *)(b))

/* Accès atomiques sur des champs ordinaires (alignés) de la région partagée, par taille.
 * Clang/GCC : builtins __atomic ; MSVC : primitives de winnt.h (x64 et ARM64). */
#if defined(_MSC_VER) && !defined(__clang__)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#define LOAD_ACQ32(p) ((uint32_t)ReadAcquire((LONG const volatile *)(p)))
#define LOAD_RLX32(p) ((uint32_t)ReadNoFence((LONG const volatile *)(p)))
#define STORE_REL32(p, v) WriteRelease((LONG volatile *)(p), (LONG)(v))
#define STORE_RLX32(p, v) WriteNoFence((LONG volatile *)(p), (LONG)(v))
#define LOAD_ACQ64(p) ((uint64_t)ReadAcquire64((LONG64 const volatile *)(p)))
#define LOAD_RLX64(p) ((uint64_t)ReadNoFence64((LONG64 const volatile *)(p)))
#define STORE_REL64(p, v) WriteRelease64((LONG64 volatile *)(p), (LONG64)(v))
#define STORE_RLX64(p, v) WriteNoFence64((LONG64 volatile *)(p), (LONG64)(v))
#define ADD_RLX64(p, v) InterlockedExchangeAdd64((LONG64 volatile *)(p), (LONG64)(v))
#define FENCE_REL() MemoryBarrier()
#define FENCE_ACQ() MemoryBarrier()
#else
#define LOAD_ACQ32(p) __atomic_load_n((const uint32_t *)(p), __ATOMIC_ACQUIRE)
#define LOAD_RLX32(p) __atomic_load_n((const uint32_t *)(p), __ATOMIC_RELAXED)
#define STORE_REL32(p, v) __atomic_store_n((uint32_t *)(p), (uint32_t)(v), __ATOMIC_RELEASE)
#define STORE_RLX32(p, v) __atomic_store_n((uint32_t *)(p), (uint32_t)(v), __ATOMIC_RELAXED)
#define LOAD_ACQ64(p) __atomic_load_n((const uint64_t *)(p), __ATOMIC_ACQUIRE)
#define LOAD_RLX64(p) __atomic_load_n((const uint64_t *)(p), __ATOMIC_RELAXED)
#define STORE_REL64(p, v) __atomic_store_n((uint64_t *)(p), (uint64_t)(v), __ATOMIC_RELEASE)
#define STORE_RLX64(p, v) __atomic_store_n((uint64_t *)(p), (uint64_t)(v), __ATOMIC_RELAXED)
#define ADD_RLX64(p, v) __atomic_fetch_add((uint64_t *)(p), (uint64_t)(v), __ATOMIC_RELAXED)
#define FENCE_REL() __atomic_thread_fence(__ATOMIC_RELEASE)
#define FENCE_ACQ() __atomic_thread_fence(__ATOMIC_ACQUIRE)
#endif

static int valid_params(uint32_t ring_frames, uint32_t c0, uint32_t c1) {
    return ring_frames >= 64 && ring_frames <= LW_SHM_MAX_RING_FRAMES && (ring_frames & (ring_frames - 1)) == 0 &&
           c0 <= LW_SHM_MAX_CHANNELS && c1 <= LW_SHM_MAX_CHANNELS;
}

size_t lw_shm_size(uint32_t ring_frames, uint32_t c0, uint32_t c1) {
    if (!valid_params(ring_frames, c0, c1)) {
        return 0;
    }
    return LW_SHM_HEADER_BYTES + (size_t)ring_frames * ((size_t)c0 + c1) * sizeof(float);
}

int lw_shm_init(void *base, size_t size, uint32_t sample_rate, uint32_t ring_frames, uint32_t c0, uint32_t c1,
                const lw_host_clock *clock) {
    size_t need = lw_shm_size(ring_frames, c0, c1);
    if (base == NULL || need == 0 || size < need) {
        return -1;
    }
    memset(base, 0, need);
    lw_shm_header *h = HDR(base);
    h->version = LW_SHM_VERSION;
    h->header_bytes = LW_SHM_HEADER_BYTES;
    h->sample_rate = sample_rate;
    h->ring_frames = ring_frames;
    h->channels[LW_TO_NET] = c0;
    h->channels[LW_FROM_NET] = c1;
    h->total_bytes = need;
    h->clock_rate_scalar = 1.0;
    if (clock != NULL) {
        h->host_clock_id = clock->id;
        h->host_ns_numer = clock->ns_numer;
        h->host_ns_denom = clock->ns_denom;
    }
    /* La magie est écrite en dernier : une région à moitié initialisée n'est jamais valide. */
    STORE_REL32(&h->magic, LW_SHM_MAGIC);
    return 0;
}

void lw_shm_host_clock(const void *base, lw_host_clock *clock) {
    const lw_shm_header *h = CHDR(base);
    clock->id = h->host_clock_id;
    clock->ns_numer = h->host_ns_numer;
    clock->ns_denom = h->host_ns_denom;
}

int lw_shm_validate(const void *base, size_t size) {
    if (base == NULL || size < LW_SHM_HEADER_BYTES) {
        return -1;
    }
    const lw_shm_header *h = CHDR(base);
    if (LOAD_ACQ32(&h->magic) != LW_SHM_MAGIC || h->version != LW_SHM_VERSION || h->header_bytes != LW_SHM_HEADER_BYTES) {
        return -1;
    }
    size_t need = lw_shm_size(h->ring_frames, h->channels[0], h->channels[1]);
    return (need != 0 && need == h->total_bytes && need <= size) ? 0 : -1;
}

static float *ring_data(void *base, int dir) {
    lw_shm_header *h = HDR(base);
    size_t off = LW_SHM_HEADER_BYTES;
    if (dir == LW_FROM_NET) {
        off += (size_t)h->ring_frames * h->channels[LW_TO_NET] * sizeof(float);
    }
    return (float *)((char *)base + off);
}

uint32_t lw_ring_writable(const void *base, int dir) {
    const lw_shm_header *h = CHDR(base);
    if (dir != LW_TO_NET && dir != LW_FROM_NET) {
        return 0;
    }
    uint64_t w = LOAD_RLX64(&h->ring[dir].write_pos);
    uint64_t r = LOAD_ACQ64(&h->ring[dir].read_pos);
    uint64_t used = w - r;
    return used >= h->ring_frames ? 0 : (uint32_t)(h->ring_frames - used);
}

uint32_t lw_ring_readable(const void *base, int dir) {
    const lw_shm_header *h = CHDR(base);
    if (dir != LW_TO_NET && dir != LW_FROM_NET) {
        return 0;
    }
    uint64_t w = LOAD_ACQ64(&h->ring[dir].write_pos);
    uint64_t r = LOAD_RLX64(&h->ring[dir].read_pos);
    uint64_t used = w - r;
    return used > h->ring_frames ? h->ring_frames : (uint32_t)used;
}

uint32_t lw_ring_write(void *base, int dir, const float *src, uint32_t frames) {
    lw_shm_header *h = HDR(base);
    if ((dir != LW_TO_NET && dir != LW_FROM_NET) || src == NULL) {
        return 0;
    }
    uint32_t ch = h->channels[dir];
    uint32_t n = lw_ring_writable(base, dir);
    if (n > frames) {
        n = frames;
    }
    if (n < frames) {
        ADD_RLX64(&h->ring[dir].overruns, (uint64_t)(frames - n));
    }
    if (ch == 0 || n == 0) {
        return n;
    }
    uint64_t w = LOAD_RLX64(&h->ring[dir].write_pos);
    uint32_t mask = h->ring_frames - 1;
    uint32_t idx = (uint32_t)(w & mask);
    uint32_t first = h->ring_frames - idx < n ? h->ring_frames - idx : n;
    float *data = ring_data(base, dir);
    memcpy(data + (size_t)idx * ch, src, (size_t)first * ch * sizeof(float));
    memcpy(data, src + (size_t)first * ch, (size_t)(n - first) * ch * sizeof(float));
    STORE_REL64(&h->ring[dir].write_pos, w + n);
    return n;
}

uint32_t lw_ring_read(void *base, int dir, float *dst, uint32_t frames) {
    lw_shm_header *h = HDR(base);
    if ((dir != LW_TO_NET && dir != LW_FROM_NET) || dst == NULL) {
        return 0;
    }
    uint32_t ch = h->channels[dir];
    uint32_t n = lw_ring_readable(base, dir);
    if (n > frames) {
        n = frames;
    }
    if (n < frames) {
        ADD_RLX64(&h->ring[dir].underruns, (uint64_t)(frames - n));
        memset(dst + (size_t)n * ch, 0, (size_t)(frames - n) * ch * sizeof(float));
    }
    if (ch == 0 || n == 0) {
        return n;
    }
    uint64_t r = LOAD_RLX64(&h->ring[dir].read_pos);
    uint32_t mask = h->ring_frames - 1;
    uint32_t idx = (uint32_t)(r & mask);
    uint32_t first = h->ring_frames - idx < n ? h->ring_frames - idx : n;
    const float *data = ring_data(base, dir);
    memcpy(dst, data + (size_t)idx * ch, (size_t)first * ch * sizeof(float));
    memcpy(dst + (size_t)first * ch, data, (size_t)(n - first) * ch * sizeof(float));
    STORE_REL64(&h->ring[dir].read_pos, r + n);
    return n;
}

void lw_ring_counters(const void *base, int dir, uint64_t *w, uint64_t *r, uint64_t *over, uint64_t *under) {
    const lw_shm_header *h = CHDR(base);
    if (dir != LW_TO_NET && dir != LW_FROM_NET) {
        *w = *r = *over = *under = 0;
        return;
    }
    *w = LOAD_ACQ64(&h->ring[dir].write_pos);
    *r = LOAD_ACQ64(&h->ring[dir].read_pos);
    *over = LOAD_RLX64(&h->ring[dir].overruns);
    *under = LOAD_RLX64(&h->ring[dir].underruns);
}

void lw_clock_publish(void *base, uint64_t host_time, uint64_t sample_time, double rate_scalar) {
    lw_shm_header *h = HDR(base);
    uint32_t seq = LOAD_RLX32(&h->clock_seq);
    STORE_RLX32(&h->clock_seq, seq + 1); /* impair : écriture en cours */
    FENCE_REL();
    STORE_RLX64(&h->clock_host_time, host_time);
    STORE_RLX64(&h->clock_sample_time, sample_time);
    uint64_t bits;
    memcpy(&bits, &rate_scalar, sizeof bits);
    STORE_RLX64(&h->clock_rate_scalar, bits);
    STORE_RLX32(&h->clock_valid, 1u);
    STORE_REL32(&h->clock_seq, seq + 2);
}

int lw_clock_read(const void *base, uint64_t *host_time, uint64_t *sample_time, double *rate_scalar) {
    const lw_shm_header *h = CHDR(base);
    for (int attempt = 0; attempt < 1000; attempt++) {
        uint32_t s1 = LOAD_ACQ32(&h->clock_seq);
        if (s1 & 1u) {
            continue;
        }
        uint32_t valid = LOAD_RLX32(&h->clock_valid);
        uint64_t ht = LOAD_RLX64(&h->clock_host_time);
        uint64_t st = LOAD_RLX64(&h->clock_sample_time);
        uint64_t bits = LOAD_RLX64(&h->clock_rate_scalar);
        FENCE_ACQ();
        if (LOAD_RLX32(&h->clock_seq) == s1) {
            if (!valid) {
                return -1;
            }
            *host_time = ht;
            *sample_time = st;
            memcpy(rate_scalar, &bits, sizeof bits);
            return 0;
        }
    }
    return -1;
}

uint32_t lw_ring_skip(void *base, int dir, uint32_t frames) {
    lw_shm_header *h = HDR(base);
    if (dir != LW_TO_NET && dir != LW_FROM_NET) {
        return 0;
    }
    uint32_t n = lw_ring_readable(base, dir);
    if (frames < n) {
        n = frames;
    }
    uint64_t r = LOAD_RLX64(&h->ring[dir].read_pos);
    STORE_REL64(&h->ring[dir].read_pos, r + n);
    return n;
}
