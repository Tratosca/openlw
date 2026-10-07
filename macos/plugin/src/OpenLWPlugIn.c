/*
 * HAL plugin (AudioServerPlugIn) for macOS Livewire / AES67 driver.
 *
 * Objects according to app-selected layout ("geometry" response, "layout"):
 * - duplex: one multichannel “OpenLW” device (2), with “from network” input stream (3) and
 *   “to network” output stream (4);
 * - multi: numbered devices “OpenLW In n” (100 + 2(n−1), input stream +1) and “OpenLW Out n”
 *   (200 + 2(n−1), output stream +1), n = 1..16, each as wide as its source (1, 2, or 8
 *   channels: "in_widths" / "out_widths"). UIDs are stable per slot
 *   (fr.francois-brille.openlw.device.in.n / .out.n).
 * Each device has its own I/O clock; they share the daemon region, one ring per device
 * (lw_shm.h: TO_NET rings, then FROM_NET rings). Layout and device-count changes modify the
 * plugin device list (PropertiesChanged). Single format: interleaved float32, 48 kHz (Livewire).
 *
 * Audio: exchanged with lw-daemon through daemon/lw-sys/csrc/lw_shm.h shared region,
 * obtained at first StartIO through XPC ("attach", Mach service fr.francois-brille.openlw.daemon,
 * declared in AudioServerPlugIn_MachServices). Without daemon, devices remain present and silent.
 *
 * Geometry: set by daemon (app settings). Monitoring queue polls "geometry" every 2 s:
 * - a device whose width changes gets a host configuration change request;
 *   PerformDeviceConfigurationChange applies the new width, host rereads properties;
 * - a new region generation (daemon recreated the region) detaches the region and reattaches
 *   at once if I/O runs: devices whose width is unchanged resume without reconfiguration.
 * Each I/O operation checks that its ring has the device's width (silence otherwise): a
 * device awaiting reconfiguration never reads or writes a ring of another width.
 * Same queue attaches region if I/O runs without it (daemon started after application).
 *
 * Names: "geometry" also contains device names ("name", "in_device_names",
 * "out_device_names") and channel names ("input_names" / "output_names", devices
 * concatenated in order). Changes notify host (PropertiesChanged) without configuration change.
 *
 * Input latency: bounded here on reader side, because only plugin knows host-requested
 * block size (512–4096 frames and beyond). Daemon fills ring while space remains. Before each
 * N-frame read, plugin waits for N + margin (priming, silence while waiting), then discards
 * excess beyond N + 2 × margin (late audio, restarted I/O). Margin follows
 * app latency preset ("geometry" response, "input_margin": 128, 256, or 512 frames).
 *
 * Real-time rules (GetZeroTimeStamp, Begin/Do/EndIOOperation): no locks, allocations, or blocking
 * system calls; region published through atomic pointer.
 *
 * Minimum: macOS 10.13 (x86_64), 11.0 (arm64). No newer symbols used.
 */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <dispatch/dispatch.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lw_shm.h"
#include "lw_sys.h"

/* ---------- Constants ---------- */

#define LW_BUNDLE_ID "fr.francois-brille.openlw.driver"
#define LW_SERVICE "fr.francois-brille.openlw.daemon"
#define LW_DEVICE_UID "fr.francois-brille.openlw.device"
#define LW_MODEL_UID "fr.francois-brille.openlw.model"
#define LW_SAMPLE_RATE 48000.0
#define LW_ZERO_TS_PERIOD 16384u
#define LW_DEFAULT_CHANNELS 2u
#define LW_MONITOR_PERIOD_NS (2ull * NSEC_PER_SEC)
/* Input-ring margin beyond one block: absorb daemon pacing (1 ms) and host pacing.
 * Default replaced by daemon value (latency preset), bounded to [64, 2048]. */
#define LW_IN_MARGIN 256u
static _Atomic uint32_t gInMargin = LW_IN_MARGIN;
#define LW_ELEMENT_MAIN 0u /* kAudioObjectPropertyElementMain (12.0+) == Master */

/* Numbered devices per direction (multi layout). */
#define LW_MAX_NUMBERED 16
/* Device slots: 0 duplex, 1..16 “OpenLW In n”, 17..32 “OpenLW Out n”. */
#define LW_SLOTS (1 + 2 * LW_MAX_NUMBERED)
#define LW_FIRST_OUT (1 + LW_MAX_NUMBERED)
/* Channel names per direction, all devices concatenated (16 devices × 8 channels). */
#define LW_MAX_NAMES 128

enum {
    kObj_PlugIn = kAudioObjectPlugInObject,
    kObj_Device = 2,     /* duplex */
    kObj_StreamIn = 3,   /* Network → applications (input) */
    kObj_StreamOut = 4,  /* Applications → network (output) */
    kObj_InBase = 100,   /* “OpenLW In n”: 100 + 2(n−1), stream + 1 */
    kObj_OutBase = 200,  /* “OpenLW Out n”: 200 + 2(n−1), stream + 1 */
};

/* Device slot of a device object, or -1. */
static int dev_index(AudioObjectID id) {
    if (id == kObj_Device) {
        return 0;
    }
    if (id >= kObj_InBase && id < kObj_InBase + 2 * LW_MAX_NUMBERED && (id - kObj_InBase) % 2 == 0) {
        return 1 + (int)(id - kObj_InBase) / 2;
    }
    if (id >= kObj_OutBase && id < kObj_OutBase + 2 * LW_MAX_NUMBERED && (id - kObj_OutBase) % 2 == 0) {
        return LW_FIRST_OUT + (int)(id - kObj_OutBase) / 2;
    }
    return -1;
}
static AudioObjectID dev_id(int idx) {
    return idx == 0 ? kObj_Device
           : idx < LW_FIRST_OUT ? (AudioObjectID)(kObj_InBase + 2 * (idx - 1))
                                : (AudioObjectID)(kObj_OutBase + 2 * (idx - LW_FIRST_OUT));
}
static Boolean idx_has_in(int idx) { return idx >= 0 && idx < LW_FIRST_OUT; }
static Boolean idx_has_out(int idx) { return idx == 0 || (idx >= LW_FIRST_OUT && idx < LW_SLOTS); }
static AudioObjectID dev_stream_in(int idx) {
    return idx == 0 ? kObj_StreamIn : idx_has_in(idx) ? dev_id(idx) + 1 : 0;
}
static AudioObjectID dev_stream_out(int idx) {
    return idx == 0 ? kObj_StreamOut : idx_has_out(idx) ? dev_id(idx) + 1 : 0;
}
/* Owning device slot of a stream object, or -1. */
static int stream_owner(AudioObjectID id) {
    if (id == kObj_StreamIn || id == kObj_StreamOut) {
        return 0;
    }
    return (id % 2 == 1) ? dev_index(id - 1) : -1;
}
static Boolean is_stream_in(AudioObjectID id) {
    int o = stream_owner(id);
    return o >= 0 && dev_stream_in(o) == id;
}
static Boolean is_stream_out(AudioObjectID id) {
    int o = stream_owner(id);
    return o >= 0 && dev_stream_out(o) == id;
}

/* ---------- State ---------- */

static AudioServerPlugInHostRef gHost = NULL;
static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gRefCount = 0;
static Float64 gHostTicksPerFrame = 0;

/* Each device's clock, I/O, and channel counts (slot index). Counts change only in
 * PerformDeviceConfigurationChange (host I/O stopped) or when a device is published. */
typedef struct {
    UInt32 io;
    UInt64 anchor;
    UInt64 count;
    UInt32 ch_in, ch_out;     /* Current format (0: direction absent) */
    UInt32 pend_in, pend_out; /* Width awaiting configuration change */
} lw_dev_state;
static lw_dev_state gDev[LW_SLOTS];
/* Layout (protected by gLock): multi, numbered device counts. */
static int gMulti = 0;
static int gNumIn = 0, gNumOut = 0;

static Boolean dev_published_locked(int idx) {
    if (idx == 0) {
        return !gMulti;
    }
    if (!gMulti || idx < 0) {
        return false;
    }
    return idx < LW_FIRST_OUT ? idx <= gNumIn : idx - LW_FIRST_OUT < gNumOut;
}

static UInt32 io_running_locked(void) {
    UInt32 n = 0;
    for (int i = 0; i < LW_SLOTS; i++) {
        n += gDev[i].io;
    }
    return n;
}

/* Active shared region (or NULL): read lock-free by I/O thread. */
static _Atomic(void *) gRegion = NULL;
static size_t gRegionSize = 0;
static void *gShmemObject = NULL;
static lw_client *gClient = NULL;
/* Test XPC endpoint (same-process harness); NULL in production. */
static void *gTestEndpoint = NULL;
/* Harness: monitoring timer is inert; harness calls lw_plugin_test_poll. */
static int gTestMode = 0;

/* Geometry monitoring (dedicated serial queue, outside real-time). */
static dispatch_queue_t gMonitorQueue = NULL;
static dispatch_source_t gMonitorTimer = NULL;
static lw_client *gMonClient = NULL;
static uint64_t gAttachedGeneration = 0;
/* Published names (protected by gLock; NULL = default name). */
#define LW_NAME_BYTES 512
static CFStringRef gDevName[LW_SLOTS];
/* Channel names per device and direction (0 input, 1 output). */
static CFStringRef gChName[LW_SLOTS][2][LW_SHM_MAX_CHANNELS];

/* Input primed per device (I/O thread; reset at I/O start and attachment). */
static _Atomic int gInPrimed[LW_SLOTS];
/* Requested configuration change per device (bit = slot index). */
static uint64_t gChangeRequested = 0;

static void plog(int level, const char *msg) {
    lw_log(level, "plugin", msg);
}

/* ---------- Daemon connection (outside real-time) ---------- */

/* Read integer following "key": in daemon JSON response (known format, no ambiguous nesting). */
static int json_u64(const char *json, const char *key, uint64_t *out) {
    char pat[64];
    snprintf(pat, sizeof pat, "\"%s\":", key);
    const char *p = strstr(json, pat);
    if (p == NULL) {
        return -1;
    }
    p += strlen(pat);
    while (*p == ' ') {
        p++;
    }
    char *end = NULL;
    unsigned long long v = strtoull(p, &end, 10);
    if (end == p) {
        return -1;
    }
    *out = v;
    return 0;
}

/* JSON string starting just after opening quote, copied as UTF-8 into out (truncated to cap).
 * Return position after closing quote, or NULL if malformed. */
static const char *json_parse_str(const char *p, char *out, size_t cap) {
    size_t n = 0;
    while (*p && *p != '"') {
        unsigned c = (unsigned char)*p++;
        if (c == '\\') {
            char e = *p++;
            switch (e) {
            case 'n': c = '\n'; break;
            case 't': c = '\t'; break;
            case 'r': c = '\r'; break;
            case 'b': c = '\b'; break;
            case 'f': c = '\f'; break;
            case 'u': {
                unsigned v = 0;
                for (int i = 0; i < 4; i++) {
                    unsigned h = (unsigned char)*p++;
                    v <<= 4;
                    if (h >= '0' && h <= '9') {
                        v |= h - '0';
                    } else if ((h | 32) >= 'a' && (h | 32) <= 'f') {
                        v |= (h | 32) - 'a' + 10;
                    } else {
                        return NULL;
                    }
                }
                /* serde escapes only control characters: basic multilingual plane suffices. */
                unsigned char u[3];
                int k = 0;
                if (v < 0x80) {
                    u[k++] = (unsigned char)v;
                } else if (v < 0x800) {
                    u[k++] = (unsigned char)(0xC0 | (v >> 6));
                    u[k++] = (unsigned char)(0x80 | (v & 0x3F));
                } else {
                    u[k++] = (unsigned char)(0xE0 | (v >> 12));
                    u[k++] = (unsigned char)(0x80 | ((v >> 6) & 0x3F));
                    u[k++] = (unsigned char)(0x80 | (v & 0x3F));
                }
                for (int i = 0; i < k; i++) {
                    if (n + 1 < cap) {
                        out[n++] = (char)u[i];
                    }
                }
                continue;
            }
            case 0: return NULL;
            default: c = (unsigned char)e; break;
            }
        }
        if (n + 1 < cap) {
            out[n++] = (char)c;
        }
    }
    if (*p != '"') {
        return NULL;
    }
    out[n] = 0;
    return p + 1;
}

/* Retained CF string; NULL if empty or invalid (truncated UTF-8). */
static CFStringRef cf_name(const char *s) {
    return *s ? CFStringCreateWithCString(NULL, s, kCFStringEncodingUTF8) : NULL;
}

/* String value for "key"; NULL if absent. */
static CFStringRef json_name(const char *json, const char *key) {
    char pat[64], buf[LW_NAME_BYTES];
    snprintf(pat, sizeof pat, "\"%s\":\"", key);
    const char *p = strstr(json, pat);
    if (p == NULL || json_parse_str(p + strlen(pat), buf, sizeof buf) == NULL) {
        return NULL;
    }
    return cf_name(buf);
}

/* String array for "key" into dst[max] (unread entries: NULL). */
static void json_names(const char *json, const char *key, CFStringRef *dst, size_t max) {
    char pat[64], buf[LW_NAME_BYTES];
    memset(dst, 0, max * sizeof *dst);
    snprintf(pat, sizeof pat, "\"%s\":[", key);
    const char *p = strstr(json, pat);
    if (p == NULL) {
        return;
    }
    p += strlen(pat);
    for (size_t i = 0; i < max;) {
        while (*p == ' ' || *p == ',') {
            p++;
        }
        if (*p != '"') {
            return;
        }
        p = json_parse_str(p + 1, buf, sizeof buf);
        if (p == NULL) {
            return;
        }
        dst[i++] = cf_name(buf);
    }
}

static Boolean same_name(CFStringRef a, CFStringRef b) {
    return (a == NULL && b == NULL) || (a != NULL && b != NULL && CFEqual(a, b));
}

static void replace_name(CFStringRef *slot, CFStringRef v) {
    CFStringRef old = *slot;
    *slot = v;
    if (old) {
        CFRelease(old);
    }
}

/* Integer array for "key" into dst[max]; return count read (0 if absent). */
static int json_u32s(const char *json, const char *key, UInt32 *dst, int max) {
    char pat[64];
    snprintf(pat, sizeof pat, "\"%s\":[", key);
    const char *p = strstr(json, pat);
    if (p == NULL) {
        return 0;
    }
    p += strlen(pat);
    int n = 0;
    while (n < max) {
        while (*p == ' ' || *p == ',') {
            p++;
        }
        char *end = NULL;
        unsigned long v = strtoul(p, &end, 10);
        if (end == p) {
            break;
        }
        dst[n++] = (UInt32)v;
        p = end;
    }
    return n;
}

/* Default device name (retained). */
static CFStringRef default_name(int idx) {
    if (idx == 0) {
        return CFRetain(CFSTR("OpenLW"));
    }
    return idx < LW_FIRST_OUT ? CFStringCreateWithFormat(NULL, NULL, CFSTR("OpenLW In %d"), idx)
                              : CFStringCreateWithFormat(NULL, NULL, CFSTR("OpenLW Out %d"), idx - LW_FIRST_OUT + 1);
}

/* Device UID (retained). */
static CFStringRef dev_uid(int idx) {
    if (idx == 0) {
        return CFRetain(CFSTR(LW_DEVICE_UID));
    }
    return idx < LW_FIRST_OUT
               ? CFStringCreateWithFormat(NULL, NULL, CFSTR(LW_DEVICE_UID ".in.%d"), idx)
               : CFStringCreateWithFormat(NULL, NULL, CFSTR(LW_DEVICE_UID ".out.%d"), idx - LW_FIRST_OUT + 1);
}

/* Names from one geometry response (owned, NULL = default). */
typedef struct {
    CFStringRef dev[LW_SLOTS];
    CFStringRef ch[LW_SLOTS][2][LW_SHM_MAX_CHANNELS];
} lw_names;

static void names_free(lw_names *n) {
    for (int i = 0; i < LW_SLOTS; i++) {
        if (n->dev[i]) {
            CFRelease(n->dev[i]);
        }
        for (int d = 0; d < 2; d++) {
            for (int c = 0; c < (int)LW_SHM_MAX_CHANNELS; c++) {
                if (n->ch[i][d][c]) {
                    CFRelease(n->ch[i][d][c]);
                }
            }
        }
    }
}

/* Spread concatenated channel names over devices `first`..`first + count − 1` (widths w). */
static void spread(CFStringRef *flat, int nflat, lw_names *n, int dir, int first, const UInt32 *w, int count) {
    int k = 0;
    for (int i = 0; i < count; i++) {
        for (UInt32 c = 0; c < w[i]; c++, k++) {
            if (k < nflat && c < LW_SHM_MAX_CHANNELS) {
                n->ch[first + i][dir][c] = flat[k];
                flat[k] = NULL;
            }
        }
    }
    for (int j = 0; j < nflat; j++) {
        if (flat[j]) {
            CFRelease(flat[j]);
        }
    }
}

/* Replace published names (takes ownership). Return the mask of renamed devices (bit =
 * slot); *ch_in / *ch_out: devices whose input / output channel names changed. */
static uint64_t set_names_locked(lw_names *n, uint64_t *ch_in, uint64_t *ch_out) {
    uint64_t renamed = 0;
    *ch_in = *ch_out = 0;
    for (int i = 0; i < LW_SLOTS; i++) {
        if (!same_name(n->dev[i], gDevName[i])) {
            renamed |= 1ull << i;
        }
        replace_name(&gDevName[i], n->dev[i]);
        n->dev[i] = NULL;
        for (int d = 0; d < 2; d++) {
            for (int c = 0; c < (int)LW_SHM_MAX_CHANNELS; c++) {
                if (!same_name(n->ch[i][d][c], gChName[i][d][c])) {
                    if (d == 0) {
                        *ch_in |= 1ull << i;
                    } else {
                        *ch_out |= 1ull << i;
                    }
                }
                replace_name(&gChName[i][d][c], n->ch[i][d][c]);
                n->ch[i][d][c] = NULL;
            }
        }
    }
    return renamed;
}

/* Detached region, unmapped only on next detach: one device's I/O may still read the
 * region when it is detached (monitoring, configuration change of another device). */
static void *gStaleRegion = NULL;
static size_t gStaleSize = 0;
static void *gStaleObject = NULL;

static void detach_locked(void) {
    void *region = atomic_exchange(&gRegion, NULL);
    if (region || gShmemObject) {
        if (gStaleRegion) {
            lw_shm_unmap(gStaleRegion, gStaleSize);
        }
        if (gStaleObject) {
            lw_xpc_release(gStaleObject);
        }
        gStaleRegion = region;
        gStaleSize = gRegionSize;
        gStaleObject = gShmemObject;
        gRegionSize = 0;
        gShmemObject = NULL;
    }
    if (gClient) {
        lw_xpc_client_close(gClient);
        gClient = NULL;
    }
}

static void attach_locked(void) {
    if (atomic_load(&gRegion) != NULL) {
        return;
    }
    detach_locked();
    gClient = gTestEndpoint ? lw_xpc_client_endpoint(gTestEndpoint) : lw_xpc_client_mach(LW_SERVICE, 1);
    if (gClient == NULL) {
        plog(3, "cannot open the XPC connection to the daemon");
        return;
    }
    const char *err = NULL;
    void *obj = NULL;
    char *reply = lw_xpc_call_shmem(gClient, "{\"cmd\":\"attach\"}", &err, &obj);
    if (reply == NULL || obj == NULL) {
        plog(3, err ? err : "the daemon did not provide a shared region");
        if (reply) {
            lw_free(reply);
        }
        if (obj) {
            lw_xpc_release(obj);
        }
        detach_locked();
        return;
    }
    uint64_t generation = 0;
    json_u64(reply, "generation", &generation);
    lw_free(reply);
    size_t size = 0;
    void *base = lw_shm_map(obj, &size);
    lw_host_clock clock = {0, 0, 0};
    if (base != NULL && lw_shm_validate(base, size) == 0) {
        lw_shm_host_clock(base, &clock);
    }
    /* Published clock must use mach_absolute_time ticks, GetZeroTimeStamp's timebase.
     * Ring widths are checked per device at each I/O operation. */
    if (base == NULL || clock.id != LW_CLOCK_MACH || lw_shm_sample_rate(base) != (uint32_t)LW_SAMPLE_RATE) {
        plog(3, "invalid shared region (magic, version, size, sample rate, or host clock)");
        lw_shm_unmap(base, size);
        lw_xpc_release(obj);
        detach_locked();
        return;
    }
    gShmemObject = obj;
    gRegionSize = size;
    gAttachedGeneration = generation;
    for (int i = 0; i < LW_SLOTS; i++) {
        atomic_store(&gInPrimed[i], 0);
    }
    atomic_store(&gRegion, base);
    plog(2, "daemon shared region attached");
}

/* Ring of a device in `region` for a direction, or -1 if absent or of another width.
 * Order (lw_shm.h): TO_NET rings (Out 1..M, or the duplex output), then FROM_NET rings. */
static int device_ring(const void *region, int idx, int in, UInt32 channels) {
    uint32_t count = lw_shm_ring_count(region), outs = 0;
    while (outs < count && lw_ring_dir(region, outs) == LW_TO_NET) {
        outs++;
    }
    uint32_t ring;
    if (in) {
        ring = outs + (uint32_t)(idx == 0 ? 0 : idx - 1);
        if (ring >= count) {
            return -1;
        }
    } else {
        ring = (uint32_t)(idx == 0 ? 0 : idx - LW_FIRST_OUT);
        if (ring >= outs) {
            return -1;
        }
    }
    return lw_ring_channels(region, ring) == channels && channels > 0 ? (int)ring : -1;
}

/* One geometry query; request configuration change if needed. */
static void monitor_tick(void) {
    pthread_mutex_lock(&gLock);
    if (gMonClient == NULL) {
        gMonClient = gTestEndpoint ? lw_xpc_client_endpoint(gTestEndpoint) : lw_xpc_client_mach(LW_SERVICE, 1);
    }
    lw_client *client = gMonClient;
    pthread_mutex_unlock(&gLock);
    if (client == NULL) {
        return;
    }
    const char *err = NULL;
    char *reply = lw_xpc_call(client, "{\"cmd\":\"geometry\"}", &err);
    if (reply == NULL) {
        /* Daemon absent/restarted: reconnect on next poll. */
        pthread_mutex_lock(&gLock);
        if (gMonClient == client) {
            lw_xpc_client_close(gMonClient);
            gMonClient = NULL;
        }
        pthread_mutex_unlock(&gLock);
        return;
    }
    uint64_t gen = 0, to = 0, from = 0, margin = 0;
    int ok = strstr(reply, "\"ok\":true") != NULL && json_u64(reply, "generation", &gen) == 0 &&
             json_u64(reply, "channels_to_net", &to) == 0 && json_u64(reply, "channels_from_net", &from) == 0;
    int multi = ok && strstr(reply, "\"layout\":\"multi\"") != NULL;
    /* Wanted width per slot (0: not published). */
    UInt32 want_in[LW_SLOTS] = {0}, want_out[LW_SLOTS] = {0};
    UInt32 in_w[LW_MAX_NUMBERED], out_w[LW_MAX_NUMBERED];
    int nin = 0, nout = 0;
    if (multi) {
        nin = json_u32s(reply, "in_widths", in_w, LW_MAX_NUMBERED);
        nout = json_u32s(reply, "out_widths", out_w, LW_MAX_NUMBERED);
        for (int i = 0; i < nin; i++) {
            ok &= in_w[i] >= 1 && in_w[i] <= LW_SHM_MAX_CHANNELS;
            want_in[1 + i] = in_w[i];
        }
        for (int i = 0; i < nout; i++) {
            ok &= out_w[i] >= 1 && out_w[i] <= LW_SHM_MAX_CHANNELS;
            want_out[LW_FIRST_OUT + i] = out_w[i];
        }
    } else {
        ok &= to >= 1 && to <= LW_SHM_MAX_CHANNELS && from >= 1 && from <= LW_SHM_MAX_CHANNELS;
        want_in[0] = (UInt32)from;
        want_out[0] = (UInt32)to;
    }
    if (ok && json_u64(reply, "input_margin", &margin) == 0 && margin >= 64 && margin <= 2048) {
        atomic_store_explicit(&gInMargin, (uint32_t)margin, memory_order_relaxed);
    }
    static lw_names names; /* Monitoring queue only (serial) */
    memset(&names, 0, sizeof names);
    if (ok) {
        static CFStringRef flat[LW_MAX_NAMES];
        CFStringRef devs[LW_MAX_NUMBERED];
        if (multi) {
            json_names(reply, "in_device_names", devs, LW_MAX_NUMBERED);
            for (int i = 0; i < LW_MAX_NUMBERED; i++) {
                names.dev[1 + i] = devs[i];
            }
            json_names(reply, "out_device_names", devs, LW_MAX_NUMBERED);
            for (int i = 0; i < LW_MAX_NUMBERED; i++) {
                names.dev[LW_FIRST_OUT + i] = devs[i];
            }
            json_names(reply, "input_names", flat, LW_MAX_NAMES);
            spread(flat, LW_MAX_NAMES, &names, 0, 1, in_w, nin);
            json_names(reply, "output_names", flat, LW_MAX_NAMES);
            spread(flat, LW_MAX_NAMES, &names, 1, LW_FIRST_OUT, out_w, nout);
        } else {
            names.dev[0] = json_name(reply, "name");
            UInt32 w_in = (UInt32)from, w_out = (UInt32)to;
            json_names(reply, "input_names", flat, LW_MAX_NAMES);
            spread(flat, LW_MAX_NAMES, &names, 0, 0, &w_in, 1);
            json_names(reply, "output_names", flat, LW_MAX_NAMES);
            spread(flat, LW_MAX_NAMES, &names, 1, 0, &w_out, 1);
        }
    }
    lw_free(reply);
    if (!ok) {
        names_free(&names);
        return;
    }
    uint64_t request = 0; /* Devices requiring configuration-change requests */
    pthread_mutex_lock(&gLock);
    AudioServerPlugInHostRef host = gHost;
    Boolean was[LW_SLOTS];
    for (int i = 0; i < LW_SLOTS; i++) {
        was[i] = dev_published_locked(i);
    }
    int relayout = multi != gMulti || nin != gNumIn || nout != gNumOut;
    gMulti = multi;
    gNumIn = nin;
    gNumOut = nout;
    uint64_t ch_in = 0, ch_out = 0;
    uint64_t renamed = set_names_locked(&names, &ch_in, &ch_out);
    /* Region recreated by the daemon: reattach (below); unchanged devices resume. */
    if (atomic_load(&gRegion) != NULL && gen != gAttachedGeneration) {
        detach_locked();
    }
    for (int i = 0; i < LW_SLOTS; i++) {
        lw_dev_state *dv = &gDev[i];
        if (!dev_published_locked(i)) {
            continue;
        }
        if (!was[i]) {
            /* Newly published: the host has not read its format yet. */
            dv->ch_in = dv->pend_in = want_in[i];
            dv->ch_out = dv->pend_out = want_out[i];
            gChangeRequested &= ~(1ull << i);
            continue;
        }
        if (want_in[i] != dv->ch_in || want_out[i] != dv->ch_out) {
            /* Most recent geometry wins, even with a pending request. */
            dv->pend_in = want_in[i];
            dv->pend_out = want_out[i];
            if (!(gChangeRequested & (1ull << i))) {
                request |= 1ull << i;
                gChangeRequested |= 1ull << i;
            }
        }
    }
    if (atomic_load(&gRegion) == NULL && io_running_locked() > 0) {
        attach_locked();
    }
    pthread_mutex_unlock(&gLock);
    if (host == NULL) {
        return;
    }
    if (relayout) {
        plog(2, multi ? "layout: numbered devices" : "layout: one device");
        AudioObjectPropertyAddress pa[2] = {
            {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN},
            {kAudioObjectPropertyOwnedObjects, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN},
        };
        host->PropertiesChanged(host, kObj_PlugIn, 2, pa);
    }
    for (int i = 0; i < LW_SLOTS; i++) {
        uint64_t bit = 1ull << i;
        if (!((renamed | ch_in | ch_out) & bit) || !was[i]) {
            continue; /* Newly published devices are read afresh by the host */
        }
        AudioObjectPropertyAddress addrs[3];
        UInt32 n = 0;
        if (renamed & bit) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN};
        }
        if ((ch_in & bit) && idx_has_in(i)) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyElementName, kAudioObjectPropertyScopeInput,
                                                      kAudioObjectPropertyElementWildcard};
        }
        if ((ch_out & bit) && idx_has_out(i)) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyElementName, kAudioObjectPropertyScopeOutput,
                                                      kAudioObjectPropertyElementWildcard};
        }
        if (n) {
            host->PropertiesChanged(host, dev_id(i), n, addrs);
        }
    }
    for (int i = 0; i < LW_SLOTS; i++) {
        if (request & (1ull << i)) {
            plog(2, "device width changed: configuration change requested");
            host->RequestDeviceConfigurationChange(host, dev_id(i), 0, NULL);
        }
    }
}

static void monitor_start(void) {
    gMonitorQueue = dispatch_queue_create("fr.francois-brille.openlw.plugin.monitor", DISPATCH_QUEUE_SERIAL);
    gMonitorTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, gMonitorQueue);
    if (gMonitorTimer == NULL) {
        return;
    }
    dispatch_source_set_timer(gMonitorTimer, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(NSEC_PER_SEC / 2)),
                              LW_MONITOR_PERIOD_NS, NSEC_PER_SEC / 10);
    dispatch_source_set_event_handler(gMonitorTimer, ^{
      if (!gTestMode) {
          monitor_tick();
      }
    });
    dispatch_resume(gMonitorTimer);
}

/* Test hook: force XPC endpoint (harness) and disable monitoring timer. */
__attribute__((visibility("default"))) void lw_plugin_test_use_endpoint(void *endpoint) {
    pthread_mutex_lock(&gLock);
    gTestMode = 1;
    gTestEndpoint = endpoint;
    detach_locked();
    if (gMonClient) {
        lw_xpc_client_close(gMonClient);
        gMonClient = NULL;
    }
    pthread_mutex_unlock(&gLock);
}

/* Test hook: one synchronous geometry query. */
__attribute__((visibility("default"))) void lw_plugin_test_poll(void) {
    if (gMonitorQueue) {
        dispatch_sync(gMonitorQueue, ^{
          monitor_tick();
        });
    } else {
        monitor_tick();
    }
}

/* ---------- Property utilities ---------- */

#define REQUIRE_SIZE(n)                                                                                                \
    do {                                                                                                               \
        if (inDataSize < (n)) {                                                                                        \
            return kAudioHardwareBadPropertySizeError;                                                                 \
        }                                                                                                              \
    } while (0)

static AudioStreamBasicDescription format_for(UInt32 channels) {
    AudioStreamBasicDescription f;
    memset(&f, 0, sizeof f);
    f.mSampleRate = LW_SAMPLE_RATE;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagsNativeFloatPacked;
    f.mBytesPerPacket = 4 * channels;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = 4 * channels;
    f.mChannelsPerFrame = channels;
    f.mBitsPerChannel = 32;
    return f;
}

/* Device channel count for a stream (current format). */
static UInt32 stream_channels(AudioObjectID id) {
    int o = stream_owner(id);
    if (o < 0) {
        return 0;
    }
    return is_stream_in(id) ? gDev[o].ch_in : gDev[o].ch_out;
}

/* Device published in current layout. */
static Boolean dev_published(int idx) {
    pthread_mutex_lock(&gLock);
    Boolean p = dev_published_locked(idx);
    pthread_mutex_unlock(&gLock);
    return p;
}

/* Published device object, or -1. */
static int published_index(AudioObjectID id) {
    int idx = dev_index(id);
    return idx >= 0 && dev_published(idx) ? idx : -1;
}

/* Stream of a published device. */
static Boolean stream_published(AudioObjectID id) {
    int o = stream_owner(id);
    return o >= 0 && dev_published(o) && (dev_stream_in(o) == id || dev_stream_out(o) == id);
}

/* Device channel name: owned scope, element 1..channel count. */
static Boolean element_name_valid(int idx, const AudioObjectPropertyAddress *a) {
    UInt32 n = a->mScope == kAudioObjectPropertyScopeInput && idx_has_in(idx)     ? gDev[idx].ch_in
               : a->mScope == kAudioObjectPropertyScopeOutput && idx_has_out(idx) ? gDev[idx].ch_out
                                                                                  : 0;
    return a->mElement >= 1 && a->mElement <= n && a->mElement <= LW_SHM_MAX_CHANNELS;
}

/* Device streams for scope (global: all). */
static UInt32 dev_streams(int idx, AudioObjectPropertyScope scope, AudioObjectID ids[2]) {
    UInt32 n = 0;
    if (dev_stream_in(idx) && (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeInput)) {
        ids[n++] = dev_stream_in(idx);
    }
    if (dev_stream_out(idx) && (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeOutput)) {
        ids[n++] = dev_stream_out(idx);
    }
    return n;
}

/* Devices published by plugin. */
static UInt32 published_devices(AudioObjectID ids[LW_SLOTS]) {
    pthread_mutex_lock(&gLock);
    UInt32 n = 0;
    for (int i = 0; i < LW_SLOTS; i++) {
        if (dev_published_locked(i)) {
            ids[n++] = dev_id(i);
        }
    }
    pthread_mutex_unlock(&gLock);
    return n;
}

/* ---------- IUnknown ---------- */

static HRESULT LW_QueryInterface(void *inDriver, REFIID inUUID, LPVOID *outInterface);
static ULONG LW_AddRef(void *inDriver);
static ULONG LW_Release(void *inDriver);
static OSStatus LW_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost);
static OSStatus LW_CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                                const AudioServerPlugInClientInfo *ci, AudioObjectID *outID);
static OSStatus LW_DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID id);
static OSStatus LW_AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id, const AudioServerPlugInClientInfo *ci);
static OSStatus LW_RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id,
                                      const AudioServerPlugInClientInfo *ci);
static OSStatus LW_PerformConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i);
static OSStatus LW_AbortConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i);
static Boolean LW_HasProperty(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a);
static OSStatus LW_IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                                      const AudioObjectPropertyAddress *a, Boolean *out);
static OSStatus LW_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                                       const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 *out);
static OSStatus LW_GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, UInt32 *outDataSize, void *outData);
static OSStatus LW_SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, const void *inData);
static OSStatus LW_StartIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client);
static OSStatus LW_StopIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client);
static OSStatus LW_GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, Float64 *st,
                                    UInt64 *ht, UInt64 *seed);
static OSStatus LW_WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                     Boolean *willDo, Boolean *inPlace);
static OSStatus LW_BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                    UInt32 frames, const AudioServerPlugInIOCycleInfo *info);
static OSStatus LW_DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, AudioObjectID stream, UInt32 client,
                                 UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *info, void *main,
                                 void *secondary);
static OSStatus LW_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                  UInt32 frames, const AudioServerPlugInIOCycleInfo *info);

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    LW_QueryInterface,
    LW_AddRef,
    LW_Release,
    LW_Initialize,
    LW_CreateDevice,
    LW_DestroyDevice,
    LW_AddDeviceClient,
    LW_RemoveDeviceClient,
    LW_PerformConfigChange,
    LW_AbortConfigChange,
    LW_HasProperty,
    LW_IsPropertySettable,
    LW_GetPropertyDataSize,
    LW_GetPropertyData,
    LW_SetPropertyData,
    LW_StartIO,
    LW_StopIO,
    LW_GetZeroTimeStamp,
    LW_WillDoIOOperation,
    LW_BeginIOOperation,
    LW_DoIOOperation,
    LW_EndIOOperation,
};
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriverRef = &gInterfacePtr;

static UInt32 plist_channels(CFBundleRef bundle, CFStringRef key) {
    CFTypeRef v = bundle ? CFBundleGetValueForInfoDictionaryKey(bundle, key) : NULL;
    SInt32 n = 0;
    if (v && CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue((CFNumberRef)v, kCFNumberSInt32Type, &n) &&
        n >= 1 && n <= (SInt32)LW_SHM_MAX_CHANNELS) {
        return (UInt32)n;
    }
    return LW_DEFAULT_CHANNELS;
}

/* Factory declared in CFPlugInFactories. Duplex layout until the daemon says otherwise. */
__attribute__((visibility("default"))) void *LW_Create(CFAllocatorRef allocator, CFUUIDRef requestedType) {
    (void)allocator;
    if (!CFEqual(requestedType, kAudioServerPlugInTypeUUID)) {
        return NULL;
    }
    CFBundleRef bundle = CFBundleGetBundleWithIdentifier(CFSTR(LW_BUNDLE_ID));
    gDev[0].ch_in = gDev[0].pend_in = plist_channels(bundle, CFSTR("LWChannelsFromNet"));
    gDev[0].ch_out = gDev[0].pend_out = plist_channels(bundle, CFSTR("LWChannelsToNet"));
    return gDriverRef;
}

static HRESULT LW_QueryInterface(void *inDriver, REFIID inUUID, LPVOID *outInterface) {
    if (inDriver != gDriverRef || outInterface == NULL) {
        return kAudioHardwareBadObjectError;
    }
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, inUUID);
    HRESULT result = E_NOINTERFACE;
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gLock);
        gRefCount++;
        pthread_mutex_unlock(&gLock);
        *outInterface = gDriverRef;
        result = S_OK;
    }
    CFRelease(requested);
    return result;
}

static ULONG LW_AddRef(void *inDriver) {
    if (inDriver != gDriverRef) {
        return 0;
    }
    pthread_mutex_lock(&gLock);
    ULONG n = ++gRefCount;
    pthread_mutex_unlock(&gLock);
    return n;
}

static ULONG LW_Release(void *inDriver) {
    if (inDriver != gDriverRef) {
        return 0;
    }
    pthread_mutex_lock(&gLock);
    ULONG n = gRefCount > 0 ? --gRefCount : 0;
    pthread_mutex_unlock(&gLock);
    return n;
}

/* ---------- Plugin operations ---------- */

static OSStatus LW_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost) {
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    gHost = inHost;
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    Float64 ticksPerSecond = 1e9 * (Float64)tb.denom / (Float64)tb.numer;
    gHostTicksPerFrame = ticksPerSecond / LW_SAMPLE_RATE;
    monitor_start();
    plog(2, "OpenLW plugin initialized");
    return kAudioHardwareNoError;
}

static OSStatus LW_CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc,
                                const AudioServerPlugInClientInfo *ci, AudioObjectID *outID) {
    (void)d, (void)desc, (void)ci, (void)outID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus LW_DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID id) {
    (void)d, (void)id;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus LW_AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id,
                                   const AudioServerPlugInClientInfo *ci) {
    (void)ci;
    return (d == gDriverRef && dev_index(id) >= 0) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus LW_RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID id,
                                      const AudioServerPlugInClientInfo *ci) {
    (void)ci;
    return (d == gDriverRef && dev_index(id) >= 0) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

/* Host calls with this device's I/O stopped: apply its new width. The region is not
 * detached (other devices may run); attach it if missing while I/O runs. */
static OSStatus LW_PerformConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)a, (void)i;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    if (gChangeRequested & (1ull << idx)) {
        gChangeRequested &= ~(1ull << idx);
        gDev[idx].ch_in = gDev[idx].pend_in;
        gDev[idx].ch_out = gDev[idx].pend_out;
        atomic_store(&gInPrimed[idx], 0);
        if (atomic_load(&gRegion) == NULL && io_running_locked() > 0) {
            attach_locked(); /* Host that does not stop I/O, or other running device */
        }
    }
    pthread_mutex_unlock(&gLock);
    plog(2, "device configuration applied");
    return kAudioHardwareNoError;
}

static OSStatus LW_AbortConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)a, (void)i;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    gChangeRequested &= ~(1ull << idx); /* New request on next monitoring poll */
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

/* ---------- Properties ---------- */

static Boolean LW_HasProperty(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a) {
    if (d != gDriverRef || a == NULL) {
        return false;
    }
    UInt32 size = 0;
    return LW_GetPropertyDataSize(d, id, pid, a, 0, NULL, &size) == kAudioHardwareNoError;
}

static OSStatus LW_IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                                      const AudioObjectPropertyAddress *a, Boolean *out) {
    if (!LW_HasProperty(d, id, pid, a) || out == NULL) {
        return kAudioHardwareUnknownPropertyError;
    }
    /* Everything read-only: one format, one sample rate. */
    *out = false;
    return kAudioHardwareNoError;
}

static OSStatus LW_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                                       const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 *out) {
    (void)qs, (void)q, (void)pid;
    if (d != gDriverRef || a == NULL || out == NULL) {
        return kAudioHardwareBadObjectError;
    }
    int idx = -1;
    if (id == kObj_PlugIn) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioPlugInPropertyTranslateUIDToDevice:
        case kAudioPlugInPropertyTranslateUIDToBox:
            *out = sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyManufacturer:
        case kAudioPlugInPropertyResourceBundle:
            *out = sizeof(CFStringRef);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList: {
            AudioObjectID ids[LW_SLOTS];
            *out = published_devices(ids) * sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        }
        case kAudioPlugInPropertyBoxList:
            *out = 0;
            return kAudioHardwareNoError;
        }
    } else if ((idx = published_index(id)) >= 0) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
        case kAudioDevicePropertyZeroTimeStampPeriod:
        case kAudioDevicePropertyClockIsStable:
            *out = sizeof(UInt32);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID:
            *out = sizeof(CFStringRef);
            return kAudioHardwareNoError;
        case kAudioDevicePropertyRelatedDevices:
            *out = sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioDevicePropertyStreams: {
            AudioObjectID ids[2];
            *out = dev_streams(idx, a->mScope, ids) * sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        }
        case kAudioObjectPropertyControlList:
            *out = 0;
            return kAudioHardwareNoError;
        case kAudioDevicePropertyNominalSampleRate:
            *out = sizeof(Float64);
            return kAudioHardwareNoError;
        case kAudioDevicePropertyAvailableNominalSampleRates:
            *out = sizeof(AudioValueRange);
            return kAudioHardwareNoError;
        case kAudioDevicePropertyPreferredChannelsForStereo:
            *out = 2 * sizeof(UInt32);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyElementName:
            if (element_name_valid(idx, a)) {
                *out = sizeof(CFStringRef);
                return kAudioHardwareNoError;
            }
            break;
        }
    } else if (stream_published(id)) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        case kAudioStreamPropertyLatency:
            *out = sizeof(UInt32);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyName:
            *out = sizeof(CFStringRef);
            return kAudioHardwareNoError;
        case kAudioObjectPropertyOwnedObjects:
            *out = 0;
            return kAudioHardwareNoError;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
            *out = sizeof(AudioStreamBasicDescription);
            return kAudioHardwareNoError;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            *out = sizeof(AudioStreamRangedDescription);
            return kAudioHardwareNoError;
        }
    }
    return kAudioHardwareUnknownPropertyError;
}

static OSStatus put_u32(UInt32 v, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    REQUIRE_SIZE(sizeof(UInt32));
    *(UInt32 *)outData = v;
    *outDataSize = sizeof(UInt32);
    return kAudioHardwareNoError;
}

static OSStatus put_str(CFStringRef s, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    REQUIRE_SIZE(sizeof(CFStringRef));
    *(CFStringRef *)outData = s; /* Constant string: caller releases it; CFSTR survives */
    CFRetain(s);
    *outDataSize = sizeof(CFStringRef);
    return kAudioHardwareNoError;
}

static OSStatus put_ids(const AudioObjectID *ids, UInt32 n, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    UInt32 fit = inDataSize / sizeof(AudioObjectID);
    if (fit < n) {
        n = fit;
    }
    if (n) {
        memcpy(outData, ids, n * sizeof(AudioObjectID));
    }
    *outDataSize = n * sizeof(AudioObjectID);
    return kAudioHardwareNoError;
}

/* Return a freshly created (retained) string and release it: caller owns its own retain. */
static OSStatus put_owned(CFStringRef s, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    if (s == NULL) {
        return kAudioHardwareUnspecifiedError;
    }
    OSStatus st = put_str(s, inDataSize, outDataSize, outData);
    CFRelease(s);
    return st;
}

static OSStatus LW_GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    (void)pid;
    if (d != gDriverRef || a == NULL || outDataSize == NULL || (outData == NULL && inDataSize > 0)) {
        return kAudioHardwareBadObjectError;
    }
    int idx = -1;
    if (id == kObj_PlugIn) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return put_u32(kAudioObjectClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyClass:
            return put_u32(kAudioPlugInClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwner:
            return put_u32(kAudioObjectUnknown, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyManufacturer:
            return put_str(CFSTR("François Brille"), inDataSize, outDataSize, outData);
        case kAudioPlugInPropertyResourceBundle:
            return put_str(CFSTR(""), inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList: {
            AudioObjectID ids[LW_SLOTS];
            UInt32 n = published_devices(ids);
            return put_ids(ids, n, inDataSize, outDataSize, outData);
        }
        case kAudioPlugInPropertyBoxList:
            *outDataSize = 0;
            return kAudioHardwareNoError;
        case kAudioPlugInPropertyTranslateUIDToDevice: {
            REQUIRE_SIZE(sizeof(AudioObjectID));
            if (qs != sizeof(CFStringRef) || q == NULL) {
                return kAudioHardwareBadPropertySizeError;
            }
            CFStringRef uid = *(const CFStringRef *)q;
            AudioObjectID found = kAudioObjectUnknown;
            for (int i = 0; i < LW_SLOTS && found == kAudioObjectUnknown; i++) {
                if (!dev_published(i)) {
                    continue;
                }
                CFStringRef u = dev_uid(i);
                if (u && CFEqual(uid, u)) {
                    found = dev_id(i);
                }
                if (u) {
                    CFRelease(u);
                }
            }
            *(AudioObjectID *)outData = found;
            *outDataSize = sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        }
        case kAudioPlugInPropertyTranslateUIDToBox:
            return put_u32(kAudioObjectUnknown, inDataSize, outDataSize, outData);
        }
    } else if ((idx = published_index(id)) >= 0) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return put_u32(kAudioObjectClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyClass:
            return put_u32(kAudioDeviceClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwner:
            return put_u32(kObj_PlugIn, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyName: {
            pthread_mutex_lock(&gLock);
            CFStringRef n = gDevName[idx] ? CFRetain(gDevName[idx]) : default_name(idx);
            pthread_mutex_unlock(&gLock);
            return put_owned(n, inDataSize, outDataSize, outData);
        }
        case kAudioObjectPropertyElementName: {
            if (!element_name_valid(idx, a)) {
                break;
            }
            pthread_mutex_lock(&gLock);
            CFStringRef s = gChName[idx][a->mScope == kAudioObjectPropertyScopeInput ? 0 : 1][a->mElement - 1];
            OSStatus st = put_str(s ? s : CFSTR(""), inDataSize, outDataSize, outData);
            pthread_mutex_unlock(&gLock);
            return st;
        }
        case kAudioObjectPropertyManufacturer:
            return put_str(CFSTR("François Brille"), inDataSize, outDataSize, outData);
        case kAudioDevicePropertyDeviceUID:
            return put_owned(dev_uid(idx), inDataSize, outDataSize, outData);
        case kAudioDevicePropertyModelUID:
            return put_str(CFSTR(LW_MODEL_UID), inDataSize, outDataSize, outData);
        case kAudioDevicePropertyTransportType:
            return put_u32(kAudioDeviceTransportTypeVirtual, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyRelatedDevices:
            return put_u32(id, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyClockDomain:
            return put_u32(0, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyDeviceIsAlive:
            return put_u32(1, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyDeviceIsRunning: {
            pthread_mutex_lock(&gLock);
            UInt32 running = gDev[idx].io > 0;
            pthread_mutex_unlock(&gLock);
            return put_u32(running, inDataSize, outDataSize, outData);
        }
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
            return put_u32(1, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
            return put_u32(0, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
            return put_u32(0, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyIsHidden:
            return put_u32(0, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyClockIsStable:
            return put_u32(1, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyZeroTimeStampPeriod:
            return put_u32(LW_ZERO_TS_PERIOD, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwnedObjects:
        case kAudioDevicePropertyStreams: {
            AudioObjectID ids[2];
            UInt32 n = dev_streams(idx, a->mScope, ids);
            return put_ids(ids, n, inDataSize, outDataSize, outData);
        }
        case kAudioObjectPropertyControlList:
            *outDataSize = 0;
            return kAudioHardwareNoError;
        case kAudioDevicePropertyNominalSampleRate:
            REQUIRE_SIZE(sizeof(Float64));
            *(Float64 *)outData = LW_SAMPLE_RATE;
            *outDataSize = sizeof(Float64);
            return kAudioHardwareNoError;
        case kAudioDevicePropertyAvailableNominalSampleRates: {
            if (inDataSize < sizeof(AudioValueRange)) {
                *outDataSize = 0;
                return kAudioHardwareNoError;
            }
            AudioValueRange r = {LW_SAMPLE_RATE, LW_SAMPLE_RATE};
            memcpy(outData, &r, sizeof r);
            *outDataSize = sizeof r;
            return kAudioHardwareNoError;
        }
        case kAudioDevicePropertyPreferredChannelsForStereo: {
            REQUIRE_SIZE(2 * sizeof(UInt32));
            /* Mono device: both sides of a stereo pair on its single channel. */
            UInt32 one = (idx_has_in(idx) ? gDev[idx].ch_in : gDev[idx].ch_out) < 2;
            UInt32 pair[2] = {1, one ? 1 : 2};
            memcpy(outData, pair, sizeof pair);
            *outDataSize = sizeof pair;
            return kAudioHardwareNoError;
        }
        }
    } else if (stream_published(id)) {
        Boolean in = is_stream_in(id);
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return put_u32(kAudioObjectClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyClass:
            return put_u32(kAudioStreamClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwner:
            return put_u32(dev_id(stream_owner(id)), inDataSize, outDataSize, outData);
        case kAudioObjectPropertyName:
            return put_str(in ? CFSTR("From Livewire") : CFSTR("To Livewire"), inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwnedObjects:
            *outDataSize = 0;
            return kAudioHardwareNoError;
        case kAudioStreamPropertyIsActive:
            return put_u32(1, inDataSize, outDataSize, outData);
        case kAudioStreamPropertyDirection:
            return put_u32(in ? 1 : 0, inDataSize, outDataSize, outData);
        case kAudioStreamPropertyTerminalType:
            return put_u32(kAudioStreamTerminalTypeLine, inDataSize, outDataSize, outData);
        case kAudioStreamPropertyStartingChannel:
            return put_u32(1, inDataSize, outDataSize, outData);
        case kAudioStreamPropertyLatency:
            return put_u32(0, inDataSize, outDataSize, outData);
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: {
            REQUIRE_SIZE(sizeof(AudioStreamBasicDescription));
            AudioStreamBasicDescription f = format_for(stream_channels(id));
            memcpy(outData, &f, sizeof f);
            *outDataSize = sizeof f;
            return kAudioHardwareNoError;
        }
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            if (inDataSize < sizeof(AudioStreamRangedDescription)) {
                *outDataSize = 0;
                return kAudioHardwareNoError;
            }
            AudioStreamRangedDescription r;
            r.mFormat = format_for(stream_channels(id));
            r.mSampleRateRange.mMinimum = LW_SAMPLE_RATE;
            r.mSampleRateRange.mMaximum = LW_SAMPLE_RATE;
            memcpy(outData, &r, sizeof r);
            *outDataSize = sizeof r;
            return kAudioHardwareNoError;
        }
        }
    }
    return kAudioHardwareUnknownPropertyError;
}

static OSStatus LW_SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, const void *inData) {
    (void)qs, (void)q, (void)pid;
    if (d != gDriverRef || a == NULL) {
        return kAudioHardwareBadObjectError;
    }
    /* Accept “setting” only supported values (some hosts do this systematically). */
    if (published_index(id) >= 0 && a->mSelector == kAudioDevicePropertyNominalSampleRate &&
        inDataSize == sizeof(Float64) && *(const Float64 *)inData == LW_SAMPLE_RATE) {
        return kAudioHardwareNoError;
    }
    if (stream_published(id) &&
        (a->mSelector == kAudioStreamPropertyVirtualFormat || a->mSelector == kAudioStreamPropertyPhysicalFormat) &&
        inDataSize == sizeof(AudioStreamBasicDescription)) {
        AudioStreamBasicDescription want = format_for(stream_channels(id));
        return memcmp(inData, &want, sizeof want) == 0 ? kAudioHardwareNoError
                                                       : kAudioDeviceUnsupportedFormatError;
    }
    return kAudioHardwareUnsupportedOperationError;
}

/* ---------- IO ---------- */

static OSStatus LW_StartIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)client;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    if (gDev[idx].io == 0) {
        gDev[idx].count = 0;
        gDev[idx].anchor = mach_absolute_time();
        attach_locked();
        if (idx_has_in(idx)) {
            /* Input I/O previously stopped: input ring contains stale audio (daemon
             * filled it then stopped writing). Drain it; priming waits for fresh audio. */
            atomic_store(&gInPrimed[idx], 0);
            void *region = atomic_load(&gRegion);
            int ring = region ? device_ring(region, idx, 1, gDev[idx].ch_in) : -1;
            if (ring >= 0) {
                lw_ring_skip(region, (uint32_t)ring, lw_ring_readable(region, (uint32_t)ring));
            }
        }
    }
    gDev[idx].io++;
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

static OSStatus LW_StopIO(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client) {
    (void)client;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    if (gDev[idx].io > 0) {
        gDev[idx].io--;
    }
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

/* Device clock: nominal 48 kHz host clock. Network-clock synchronization
 * (region rate_scalar): future work when daemon follows PTP or Livewire clock. */
static OSStatus LW_GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, Float64 *st,
                                    UInt64 *ht, UInt64 *seed) {
    (void)client;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    lw_dev_state *dv = &gDev[idx];
    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = gHostTicksPerFrame * (Float64)LW_ZERO_TS_PERIOD;
    UInt64 next = dv->anchor + (UInt64)((Float64)(dv->count + 1) * ticksPerPeriod);
    if (next <= now) {
        dv->count++;
    }
    *st = (Float64)(dv->count * LW_ZERO_TS_PERIOD);
    *ht = dv->anchor + (UInt64)((Float64)dv->count * ticksPerPeriod);
    *seed = 1;
    return kAudioHardwareNoError;
}

static OSStatus LW_WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                     Boolean *willDo, Boolean *inPlace) {
    (void)client;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    *willDo = (op == kAudioServerPlugInIOOperationReadInput && idx_has_in(idx)) ||
              (op == kAudioServerPlugInIOOperationWriteMix && idx_has_out(idx));
    *inPlace = true;
    return kAudioHardwareNoError;
}

static OSStatus LW_BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                    UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    (void)client, (void)op, (void)frames, (void)info;
    return (d == gDriverRef && dev_index(id) >= 0) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus LW_DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, AudioObjectID stream, UInt32 client,
                                 UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *info, void *main,
                                 void *secondary) {
    (void)client, (void)info, (void)secondary;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0 || stream_owner(stream) != idx) {
        return kAudioHardwareBadObjectError;
    }
    void *region = atomic_load_explicit(&gRegion, memory_order_acquire);
    if (op == kAudioServerPlugInIOOperationReadInput && is_stream_in(stream) && main) {
        UInt32 ch = gDev[idx].ch_in;
        int ring = region ? device_ring(region, idx, 1, ch) : -1;
        uint32_t margin = atomic_load_explicit(&gInMargin, memory_order_relaxed);
        uint32_t keep = frames + margin;
        uint32_t avail = ring >= 0 ? lw_ring_readable(region, (uint32_t)ring) : 0;
        int primed = atomic_load_explicit(&gInPrimed[idx], memory_order_relaxed);
        if (ring >= 0 && !primed && avail >= keep) {
            primed = 1;
            atomic_store_explicit(&gInPrimed[idx], 1, memory_order_relaxed);
        }
        if (ring >= 0 && primed) {
            if (avail > keep + margin) {
                lw_ring_skip(region, (uint32_t)ring, avail - keep); /* Late audio: catch up */
            }
            if (lw_ring_read(region, (uint32_t)ring, (float *)main, frames) < frames) {
                atomic_store_explicit(&gInPrimed[idx], 0, memory_order_relaxed); /* Underrun: reprime */
            }
        } else {
            memset(main, 0, (size_t)frames * ch * sizeof(float));
        }
    } else if (op == kAudioServerPlugInIOOperationWriteMix && is_stream_out(stream) && main && region) {
        int ring = device_ring(region, idx, 0, gDev[idx].ch_out);
        if (ring >= 0) {
            lw_ring_write(region, (uint32_t)ring, (const float *)main, frames);
        }
    }
    return kAudioHardwareNoError;
}

static OSStatus LW_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                  UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    (void)client, (void)op, (void)frames, (void)info;
    return (d == gDriverRef && dev_index(id) >= 0) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
