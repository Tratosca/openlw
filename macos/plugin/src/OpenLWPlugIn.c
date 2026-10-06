/*
 * Plugin HAL (AudioServerPlugIn) du driver Livewire / AES67 pour macOS.
 *
 * Objets, selon la présentation choisie dans l'app (réponse "geometry", "layout") :
 * - un périphérique duplex « OpenLW » (2) avec un flux d'entrée « depuis le réseau » (3) et un flux de
 *   sortie « vers le réseau » (4) ;
 * - ou deux périphériques : « OpenLW In » (5, flux d'entrée 6) et « OpenLW Out » (7, flux de
 *   sortie 8). Chaque périphérique a sa propre horloge d'IO ; ils partagent la région du daemon.
 * Le changement de présentation modifie la liste des périphériques du plugin (PropertiesChanged).
 * Format unique : float32 entrelacé, 48 kHz (Livewire).
 *
 * Audio : échangé avec le daemon (lw-daemon) par la région partagée de daemon/lw-sys/csrc/lw_shm.h,
 * obtenue au premier StartIO par XPC (requête "attach", service Mach fr.francois-brille.openlw.daemon,
 * déclaré dans AudioServerPlugIn_MachServices). Sans daemon, le périphérique reste présent et silencieux.
 *
 * Nombre de canaux : fixé par le daemon (réglage de l'app). Une file de surveillance interroge
 * "geometry" toutes les 2 s ; si le nombre de canaux ou la génération de la région change, elle demande
 * un changement de configuration à l'hôte. PerformDeviceConfigurationChange détache la région et
 * applique les nouveaux nombres ; l'hôte relit les propriétés et le StartIO suivant se rattache.
 * La même file rattache la région si l'IO tourne sans elle (daemon démarré après l'application).
 *
 * Noms : la réponse "geometry" porte aussi le nom des périphériques (« OpenLW », ou « OpenLW In
 * (2 - Studio A) » si l'option est activée dans l'app) et le nom de chaque canal (« 2 - Studio A G »).
 * Un changement est signalé à l'hôte (PropertiesChanged), sans changement de configuration.
 *
 * Latence d'entrée : bornée ici, côté lecteur, car seul le plugin connaît la taille de bloc demandée par
 * l'hôte (512 à 4096 trames et plus). Le daemon remplit l'anneau tant qu'il y a de la place. Avant chaque
 * lecture de N trames, le plugin attend d'en avoir N + marge (amorçage, silence en attendant), puis jette
 * l'excédent au-delà de N + 2 × marge (audio en retard, IO redémarrée). La marge suit le préréglage de
 * latence choisi dans l'app (réponse "geometry", "input_margin" : 128, 256 ou 512 trames).
 *
 * Règles temps réel (GetZeroTimeStamp, Begin/Do/EndIOOperation) : ni verrou, ni allocation, ni appel
 * système bloquant ; la région est publiée par un pointeur atomique.
 *
 * Plancher : macOS 10.13 (x86_64), 11.0 (arm64). Aucun symbole postérieur n'est utilisé.
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

/* ---------- Constantes ---------- */

#define LW_BUNDLE_ID "fr.francois-brille.openlw.driver"
#define LW_SERVICE "fr.francois-brille.openlw.daemon"
#define LW_DEVICE_UID "fr.francois-brille.openlw.device"
#define LW_DEVICE_IN_UID "fr.francois-brille.openlw.device.in"
#define LW_DEVICE_OUT_UID "fr.francois-brille.openlw.device.out"
#define LW_MODEL_UID "fr.francois-brille.openlw.model"
#define LW_SAMPLE_RATE 48000.0
#define LW_ZERO_TS_PERIOD 16384u
#define LW_DEFAULT_CHANNELS 2u
#define LW_MONITOR_PERIOD_NS (2ull * NSEC_PER_SEC)
/* Marge de l'anneau d'entrée au-delà d'un bloc : absorbe le cadencement du daemon (1 ms) et celui de l'hôte.
 * Valeur par défaut, remplacée par celle du daemon (préréglage de latence), bornée à [64, 2048]. */
#define LW_IN_MARGIN 256u
static _Atomic uint32_t gInMargin = LW_IN_MARGIN;
#define LW_ELEMENT_MAIN 0u /* kAudioObjectPropertyElementMain (12.0+) == Master */

enum {
    kObj_PlugIn = kAudioObjectPlugInObject,
    kObj_Device = 2,     /* duplex */
    kObj_StreamIn = 3,   /* réseau → applications (entrée) */
    kObj_StreamOut = 4,  /* applications → réseau (sortie) */
    kObj_DevIn = 5,      /* « OpenLW In » */
    kObj_StreamIn2 = 6,
    kObj_DevOut = 7,     /* « OpenLW Out » */
    kObj_StreamOut2 = 8,
};

/* Index de périphérique (0 duplex, 1 entrée, 2 sortie), ou -1. */
static int dev_index(AudioObjectID id) {
    return id == kObj_Device ? 0 : id == kObj_DevIn ? 1 : id == kObj_DevOut ? 2 : -1;
}
static const AudioObjectID kDevIds[3] = {kObj_Device, kObj_DevIn, kObj_DevOut};
static Boolean dev_has_in(AudioObjectID id) { return id == kObj_Device || id == kObj_DevIn; }
static Boolean dev_has_out(AudioObjectID id) { return id == kObj_Device || id == kObj_DevOut; }
static AudioObjectID dev_stream_in(AudioObjectID id) {
    return id == kObj_Device ? kObj_StreamIn : id == kObj_DevIn ? kObj_StreamIn2 : 0;
}
static AudioObjectID dev_stream_out(AudioObjectID id) {
    return id == kObj_Device ? kObj_StreamOut : id == kObj_DevOut ? kObj_StreamOut2 : 0;
}
static Boolean is_stream_in(AudioObjectID id) { return id == kObj_StreamIn || id == kObj_StreamIn2; }
static Boolean is_stream_out(AudioObjectID id) { return id == kObj_StreamOut || id == kObj_StreamOut2; }
static AudioObjectID stream_owner(AudioObjectID id) {
    return id == kObj_StreamIn2 ? kObj_DevIn : id == kObj_StreamOut2 ? kObj_DevOut : kObj_Device;
}

/* ---------- État ---------- */

static AudioServerPlugInHostRef gHost = NULL;
static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gRefCount = 0;
static UInt32 gChannelsIn = LW_DEFAULT_CHANNELS;  /* depuis le réseau */
static UInt32 gChannelsOut = LW_DEFAULT_CHANNELS; /* vers le réseau */
static Float64 gHostTicksPerFrame = 0;

/* Horloge et IO de chaque périphérique (index dev_index). */
typedef struct {
    UInt32 io;
    UInt64 anchor;
    UInt64 count;
} lw_dev_state;
static lw_dev_state gDev[3];
/* Présentation : 0 un périphérique duplex, 1 deux périphériques (protégé par gLock). */
static int gSplit = 0;

static UInt32 io_running_locked(void) {
    return gDev[0].io + gDev[1].io + gDev[2].io;
}

/* Région partagée active (ou NULL) : lue sans verrou par le thread IO. */
static _Atomic(void *) gRegion = NULL;
static size_t gRegionSize = 0;
static void *gShmemObject = NULL;
static lw_client *gClient = NULL;
/* Point d'accès XPC de test (banc d'essai dans le même processus) ; NULL en production. */
static void *gTestEndpoint = NULL;
/* Banc d'essai : la minuterie de surveillance ne fait rien, le banc appelle lw_plugin_test_poll. */
static int gTestMode = 0;

/* Surveillance de la géométrie (file série dédiée, hors temps réel). */
static dispatch_queue_t gMonitorQueue = NULL;
static dispatch_source_t gMonitorTimer = NULL;
static lw_client *gMonClient = NULL;
static uint64_t gAttachedGeneration = 0;
/* Noms publiés (protégés par gLock ; NULL = nom par défaut). */
#define LW_NAME_BYTES 512
static CFStringRef gDeviceName = NULL;
static CFStringRef gInDevName = NULL;
static CFStringRef gOutDevName = NULL;
static CFStringRef gInNames[LW_SHM_MAX_CHANNELS];
static CFStringRef gOutNames[LW_SHM_MAX_CHANNELS];

/* Entrée amorcée (thread IO ; remis à zéro au démarrage de l'IO et au rattachement). */
static _Atomic int gInPrimed = 0;
/* Changement de configuration demandé, par périphérique (bit = dev_index). */
static int gChangeRequested = 0;
static UInt32 gPendingIn = LW_DEFAULT_CHANNELS;
static UInt32 gPendingOut = LW_DEFAULT_CHANNELS;

static void plog(int level, const char *msg) {
    lw_log(level, "plugin", msg);
}

/* ---------- Connexion au daemon (hors temps réel) ---------- */

/* Lit l'entier qui suit "key": dans une réponse JSON du daemon (format connu, sans imbrication ambiguë). */
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

/* Chaîne JSON commençant juste après son guillemet ouvrant, copiée en UTF-8 dans out (tronquée à cap).
 * Retourne la position qui suit le guillemet fermant, ou NULL si la chaîne est mal formée. */
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
                /* serde n'échappe que les caractères de contrôle : plan multilingue de base suffisant. */
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

/* Chaîne CF (retenue) ; NULL si vide ou invalide (UTF-8 tronqué). */
static CFStringRef cf_name(const char *s) {
    return *s ? CFStringCreateWithCString(NULL, s, kCFStringEncodingUTF8) : NULL;
}

/* Valeur chaîne de "key" ; NULL si absente. */
static CFStringRef json_name(const char *json, const char *key) {
    char pat[64], buf[LW_NAME_BYTES];
    snprintf(pat, sizeof pat, "\"%s\":\"", key);
    const char *p = strstr(json, pat);
    if (p == NULL || json_parse_str(p + strlen(pat), buf, sizeof buf) == NULL) {
        return NULL;
    }
    return cf_name(buf);
}

/* Tableau de chaînes de "key" dans dst[max] (entrées non lues : NULL). */
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

/* Remplace les noms publiés ; retourne un masque : 1 nom du duplex, 2 canaux d'entrée, 4 canaux de
 * sortie, 8 nom de « OpenLW In », 16 nom de « OpenLW Out ». */
static int set_names_locked(CFStringRef name, CFStringRef in_dev, CFStringRef out_dev, CFStringRef *in,
                            CFStringRef *out) {
    int changed = 0;
    changed |= same_name(name, gDeviceName) ? 0 : 1;
    changed |= same_name(in_dev, gInDevName) ? 0 : 8;
    changed |= same_name(out_dev, gOutDevName) ? 0 : 16;
    for (size_t i = 0; i < LW_SHM_MAX_CHANNELS; i++) {
        changed |= same_name(in[i], gInNames[i]) ? 0 : 2;
        changed |= same_name(out[i], gOutNames[i]) ? 0 : 4;
    }
    replace_name(&gDeviceName, name);
    replace_name(&gInDevName, in_dev);
    replace_name(&gOutDevName, out_dev);
    for (size_t i = 0; i < LW_SHM_MAX_CHANNELS; i++) {
        replace_name(&gInNames[i], in[i]);
        replace_name(&gOutNames[i], out[i]);
    }
    return changed;
}

/* Région détachée, démappée seulement au détachement suivant : avec deux périphériques, l'IO de
 * l'un peut encore lire la région quand l'autre la détache (changement de configuration). */
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
        plog(3, "connexion XPC au daemon impossible");
        return;
    }
    const char *err = NULL;
    void *obj = NULL;
    char *reply = lw_xpc_call_shmem(gClient, "{\"cmd\":\"attach\"}", &err, &obj);
    if (reply == NULL || obj == NULL) {
        plog(3, err ? err : "le daemon n'a pas fourni de région partagée");
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
    /* L'horloge publiée doit être en ticks mach_absolute_time, base de GetZeroTimeStamp. */
    if (base == NULL || clock.id != LW_CLOCK_MACH) {
        plog(3, "région partagée invalide (magie, version, taille ou horloge hôte)");
        lw_shm_unmap(base, size);
        lw_xpc_release(obj);
        detach_locked();
        return;
    }
    const lw_shm_header *h = (const lw_shm_header *)base;
    if (h->channels[LW_TO_NET] != gChannelsOut || h->channels[LW_FROM_NET] != gChannelsIn ||
        h->sample_rate != (uint32_t)LW_SAMPLE_RATE) {
        /* Nombre de canaux changé côté daemon : la surveillance demandera le changement de configuration. */
        plog(3, "géométrie de la région différente de celle du périphérique (canaux ou fréquence)");
        lw_shm_unmap(base, size);
        lw_xpc_release(obj);
        detach_locked();
        return;
    }
    gShmemObject = obj;
    gRegionSize = size;
    gAttachedGeneration = generation;
    atomic_store(&gInPrimed, 0);
    atomic_store(&gRegion, base);
    plog(2, "région partagée du daemon attachée");
}

/* Une interrogation de la géométrie ; demande un changement de configuration si besoin. */
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
        /* Daemon absent ou redémarré : nouvelle connexion au prochain passage. */
        pthread_mutex_lock(&gLock);
        if (gMonClient == client) {
            lw_xpc_client_close(gMonClient);
            gMonClient = NULL;
        }
        pthread_mutex_unlock(&gLock);
        return;
    }
    uint64_t gen = 0, to = 0, from = 0;
    int ok = strstr(reply, "\"ok\":true") != NULL && json_u64(reply, "generation", &gen) == 0 &&
             json_u64(reply, "channels_to_net", &to) == 0 && json_u64(reply, "channels_from_net", &from) == 0 &&
             to >= 1 && to <= LW_SHM_MAX_CHANNELS && from >= 1 && from <= LW_SHM_MAX_CHANNELS;
    CFStringRef name = NULL, in_dev = NULL, out_dev = NULL, in[LW_SHM_MAX_CHANNELS], out[LW_SHM_MAX_CHANNELS];
    uint64_t margin = 0;
    if (ok && json_u64(reply, "input_margin", &margin) == 0 && margin >= 64 && margin <= 2048) {
        atomic_store_explicit(&gInMargin, (uint32_t)margin, memory_order_relaxed);
    }
    int split = 0;
    if (ok) {
        split = strstr(reply, "\"layout\":\"split\"") != NULL;
        name = json_name(reply, "name");
        in_dev = json_name(reply, "input_device_name");
        out_dev = json_name(reply, "output_device_name");
        json_names(reply, "input_names", in, LW_SHM_MAX_CHANNELS);
        json_names(reply, "output_names", out, LW_SHM_MAX_CHANNELS);
    }
    lw_free(reply);
    if (!ok) {
        return;
    }
    int request = 0; /* périphériques pour lesquels demander un changement de configuration */
    pthread_mutex_lock(&gLock);
    AudioServerPlugInHostRef host = gHost;
    int relayout = split != gSplit;
    gSplit = split;
    int renamed = set_names_locked(name, in_dev, out_dev, in, out);
    int attached = atomic_load(&gRegion) != NULL;
    int change = (UInt32)to != gChannelsOut || (UInt32)from != gChannelsIn || (attached && gen != gAttachedGeneration);
    int published = gSplit ? (2 | 4) : 1;
    if (change) {
        /* La géométrie la plus récente l'emporte, même si une demande est déjà en attente. */
        gPendingOut = (UInt32)to;
        gPendingIn = (UInt32)from;
        request = published & ~gChangeRequested;
        gChangeRequested |= published;
    } else if (!attached && io_running_locked() > 0) {
        attach_locked();
    }
    pthread_mutex_unlock(&gLock);
    if (host == NULL) {
        return;
    }
    if (relayout) {
        plog(2, split ? "présentation : deux périphériques" : "présentation : un périphérique");
        AudioObjectPropertyAddress pa[2] = {
            {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN},
            {kAudioObjectPropertyOwnedObjects, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN},
        };
        host->PropertiesChanged(host, kObj_PlugIn, 2, pa);
    }
    for (int i = 0; i < 3 && renamed; i++) {
        if (!(published & (1 << i))) {
            continue;
        }
        AudioObjectID dev = kDevIds[i];
        AudioObjectPropertyAddress addrs[3];
        UInt32 n = 0;
        if ((i == 0 && (renamed & 1)) || (i == 1 && (renamed & 8)) || (i == 2 && (renamed & 16))) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, LW_ELEMENT_MAIN};
        }
        if ((renamed & 2) && dev_has_in(dev)) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyElementName, kAudioObjectPropertyScopeInput,
                                                      kAudioObjectPropertyElementWildcard};
        }
        if ((renamed & 4) && dev_has_out(dev)) {
            addrs[n++] = (AudioObjectPropertyAddress){kAudioObjectPropertyElementName, kAudioObjectPropertyScopeOutput,
                                                      kAudioObjectPropertyElementWildcard};
        }
        if (n) {
            host->PropertiesChanged(host, dev, n, addrs);
        }
    }
    for (int i = 0; i < 3; i++) {
        if (request & (1 << i)) {
            plog(2, "géométrie du daemon modifiée : changement de configuration demandé");
            host->RequestDeviceConfigurationChange(host, kDevIds[i], 0, NULL);
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

/* Hook de test : force un point d'accès XPC (banc d'essai) et coupe la minuterie de surveillance. */
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

/* Hook de test : une interrogation de la géométrie, synchrone. */
__attribute__((visibility("default"))) void lw_plugin_test_poll(void) {
    if (gMonitorQueue) {
        dispatch_sync(gMonitorQueue, ^{
          monitor_tick();
        });
    } else {
        monitor_tick();
    }
}

/* ---------- Utilitaires de propriétés ---------- */

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

static Boolean is_stream(AudioObjectID id) {
    return is_stream_in(id) || is_stream_out(id);
}

static UInt32 stream_channels(AudioObjectID id) {
    return is_stream_in(id) ? gChannelsIn : gChannelsOut;
}

/* Périphérique publié dans la présentation courante (verrou pris par l'appelant ou lecture tolérée). */
static Boolean dev_published(AudioObjectID id) {
    return gSplit ? (id == kObj_DevIn || id == kObj_DevOut) : id == kObj_Device;
}

/* Nom de canal d'un périphérique : portée qu'il possède, élément 1..nombre de canaux. */
static Boolean element_name_valid(AudioObjectID dev, const AudioObjectPropertyAddress *a) {
    UInt32 n = a->mScope == kAudioObjectPropertyScopeInput && dev_has_in(dev)     ? gChannelsIn
               : a->mScope == kAudioObjectPropertyScopeOutput && dev_has_out(dev) ? gChannelsOut
                                                                                  : 0;
    return a->mElement >= 1 && a->mElement <= n && a->mElement <= LW_SHM_MAX_CHANNELS;
}

/* Flux d'un périphérique pour une portée (global : tous). */
static UInt32 dev_streams(AudioObjectID dev, AudioObjectPropertyScope scope, AudioObjectID ids[2]) {
    UInt32 n = 0;
    if (dev_stream_in(dev) && (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeInput)) {
        ids[n++] = dev_stream_in(dev);
    }
    if (dev_stream_out(dev) && (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeOutput)) {
        ids[n++] = dev_stream_out(dev);
    }
    return n;
}

/* Périphériques publiés par le plugin. */
static UInt32 published_devices(AudioObjectID ids[2]) {
    pthread_mutex_lock(&gLock);
    UInt32 n = 0;
    if (gSplit) {
        ids[n++] = kObj_DevIn;
        ids[n++] = kObj_DevOut;
    } else {
        ids[n++] = kObj_Device;
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

/* Fabrique déclarée dans CFPlugInFactories. */
__attribute__((visibility("default"))) void *LW_Create(CFAllocatorRef allocator, CFUUIDRef requestedType) {
    (void)allocator;
    if (!CFEqual(requestedType, kAudioServerPlugInTypeUUID)) {
        return NULL;
    }
    CFBundleRef bundle = CFBundleGetBundleWithIdentifier(CFSTR(LW_BUNDLE_ID));
    gChannelsIn = plist_channels(bundle, CFSTR("LWChannelsFromNet"));
    gChannelsOut = plist_channels(bundle, CFSTR("LWChannelsToNet"));
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

/* ---------- Opérations du plugin ---------- */

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
    plog(2, "plugin OpenLW initialisé");
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

/* Appelé par l'hôte, IO de ce périphérique arrêtée : il applique la nouvelle géométrie de ses sens
 * (duplex : les deux), et la région est détachée (rattachée au StartIO suivant). L'autre périphérique
 * (présentation en deux) peut tourner encore : la région détachée n'est démappée que plus tard. */
static OSStatus LW_PerformConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)a, (void)i;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    if (gChangeRequested & (1 << idx)) {
        gChangeRequested &= ~(1 << idx);
        detach_locked();
        if (dev_has_out(id)) {
            gChannelsOut = gPendingOut;
        }
        if (dev_has_in(id)) {
            gChannelsIn = gPendingIn;
        }
        if (io_running_locked() > 0) {
            attach_locked(); /* hôte qui n'arrête pas l'IO, ou autre périphérique en marche */
        }
    }
    pthread_mutex_unlock(&gLock);
    plog(2, "configuration du périphérique appliquée");
    return kAudioHardwareNoError;
}

static OSStatus LW_AbortConfigChange(AudioServerPlugInDriverRef d, AudioObjectID id, UInt64 a, void *i) {
    (void)a, (void)i;
    int idx = dev_index(id);
    if (d != gDriverRef || idx < 0) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    gChangeRequested &= ~(1 << idx); /* nouvelle demande au prochain passage de la surveillance */
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

/* ---------- Propriétés ---------- */

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
    /* Tout est en lecture seule : un seul format, une seule fréquence. */
    *out = false;
    return kAudioHardwareNoError;
}

static OSStatus LW_GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid,
                                       const AudioObjectPropertyAddress *a, UInt32 qs, const void *q, UInt32 *out) {
    (void)qs, (void)q, (void)pid;
    if (d != gDriverRef || a == NULL || out == NULL) {
        return kAudioHardwareBadObjectError;
    }
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
            AudioObjectID ids[2];
            *out = published_devices(ids) * sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        }
        case kAudioPlugInPropertyBoxList:
            *out = 0;
            return kAudioHardwareNoError;
        }
    } else if (dev_index(id) >= 0) {
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
            *out = dev_streams(id, a->mScope, ids) * sizeof(AudioObjectID);
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
            if (element_name_valid(id, a)) {
                *out = sizeof(CFStringRef);
                return kAudioHardwareNoError;
            }
            break;
        }
    } else if (is_stream(id)) {
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
    *(CFStringRef *)outData = s; /* chaîne constante : l'appelant la libère, CFSTR survit */
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

static CFStringRef dev_uid(AudioObjectID dev) {
    return dev == kObj_DevIn ? CFSTR(LW_DEVICE_IN_UID) : dev == kObj_DevOut ? CFSTR(LW_DEVICE_OUT_UID) : CFSTR(LW_DEVICE_UID);
}

static OSStatus LW_GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID id, pid_t pid, const AudioObjectPropertyAddress *a,
                                   UInt32 qs, const void *q, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    (void)pid;
    if (d != gDriverRef || a == NULL || outDataSize == NULL || (outData == NULL && inDataSize > 0)) {
        return kAudioHardwareBadObjectError;
    }
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
            AudioObjectID ids[2];
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
            pthread_mutex_lock(&gLock);
            for (int i = 0; i < 3; i++) {
                if (dev_published(kDevIds[i]) && CFEqual(uid, dev_uid(kDevIds[i]))) {
                    found = kDevIds[i];
                }
            }
            pthread_mutex_unlock(&gLock);
            *(AudioObjectID *)outData = found;
            *outDataSize = sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        }
        case kAudioPlugInPropertyTranslateUIDToBox:
            return put_u32(kAudioObjectUnknown, inDataSize, outDataSize, outData);
        }
    } else if (dev_index(id) >= 0) {
        int idx = dev_index(id);
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return put_u32(kAudioObjectClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyClass:
            return put_u32(kAudioDeviceClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwner:
            return put_u32(kObj_PlugIn, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyName: {
            pthread_mutex_lock(&gLock);
            CFStringRef n = idx == 1 ? (gInDevName ? gInDevName : CFSTR("OpenLW In"))
                            : idx == 2 ? (gOutDevName ? gOutDevName : CFSTR("OpenLW Out"))
                                       : (gDeviceName ? gDeviceName : CFSTR("OpenLW"));
            OSStatus st = put_str(n, inDataSize, outDataSize, outData);
            pthread_mutex_unlock(&gLock);
            return st;
        }
        case kAudioObjectPropertyElementName: {
            if (!element_name_valid(id, a)) {
                break;
            }
            pthread_mutex_lock(&gLock);
            CFStringRef *names = a->mScope == kAudioObjectPropertyScopeInput ? gInNames : gOutNames;
            CFStringRef s = names[a->mElement - 1];
            OSStatus st = put_str(s ? s : CFSTR(""), inDataSize, outDataSize, outData);
            pthread_mutex_unlock(&gLock);
            return st;
        }
        case kAudioObjectPropertyManufacturer:
            return put_str(CFSTR("François Brille"), inDataSize, outDataSize, outData);
        case kAudioDevicePropertyDeviceUID:
            return put_str(dev_uid(id), inDataSize, outDataSize, outData);
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
            UInt32 n = dev_streams(id, a->mScope, ids);
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
            UInt32 pair[2] = {1, 2};
            memcpy(outData, pair, sizeof pair);
            *outDataSize = sizeof pair;
            return kAudioHardwareNoError;
        }
        }
    } else if (is_stream(id)) {
        Boolean in = is_stream_in(id);
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return put_u32(kAudioObjectClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyClass:
            return put_u32(kAudioStreamClassID, inDataSize, outDataSize, outData);
        case kAudioObjectPropertyOwner:
            return put_u32(stream_owner(id), inDataSize, outDataSize, outData);
        case kAudioObjectPropertyName:
            return put_str(in ? CFSTR("Depuis Livewire") : CFSTR("Vers Livewire"), inDataSize, outDataSize, outData);
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
    /* Accepte de « fixer » les seules valeurs supportées (certains hôtes le font systématiquement). */
    if (dev_index(id) >= 0 && a->mSelector == kAudioDevicePropertyNominalSampleRate && inDataSize == sizeof(Float64) &&
        *(const Float64 *)inData == LW_SAMPLE_RATE) {
        return kAudioHardwareNoError;
    }
    if (is_stream(id) &&
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
        if (dev_has_in(id)) {
            /* IO d'entrée arrêtée jusqu'ici : l'anneau d'entrée contient de l'audio ancien (le daemon
             * l'a rempli puis a cessé d'écrire). Il est vidé ; l'amorçage attend de l'audio frais. */
            atomic_store(&gInPrimed, 0);
            void *region = atomic_load(&gRegion);
            if (region) {
                lw_ring_skip(region, LW_FROM_NET, lw_ring_readable(region, LW_FROM_NET));
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

/* Horloge du périphérique : horloge hôte à 48 kHz nominal. Asservissement sur l'horloge réseau
 * (rate_scalar de la région) : à venir, quand le daemon suivra PTP ou l'horloge Livewire. */
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
    if (d != gDriverRef || dev_index(id) < 0) {
        return kAudioHardwareBadObjectError;
    }
    *willDo = (op == kAudioServerPlugInIOOperationReadInput && dev_has_in(id)) ||
              (op == kAudioServerPlugInIOOperationWriteMix && dev_has_out(id));
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
    if (d != gDriverRef || dev_index(id) < 0 || !is_stream(stream) || stream_owner(stream) != id) {
        return kAudioHardwareBadObjectError;
    }
    void *region = atomic_load_explicit(&gRegion, memory_order_acquire);
    if (op == kAudioServerPlugInIOOperationReadInput && is_stream_in(stream) && main) {
        uint32_t margin = atomic_load_explicit(&gInMargin, memory_order_relaxed);
        uint32_t keep = frames + margin;
        uint32_t avail = region ? lw_ring_readable(region, LW_FROM_NET) : 0;
        int primed = atomic_load_explicit(&gInPrimed, memory_order_relaxed);
        if (region && !primed && avail >= keep) {
            primed = 1;
            atomic_store_explicit(&gInPrimed, 1, memory_order_relaxed);
        }
        if (region && primed) {
            if (avail > keep + margin) {
                lw_ring_skip(region, LW_FROM_NET, avail - keep); /* audio en retard : on rattrape */
            }
            if (lw_ring_read(region, LW_FROM_NET, (float *)main, frames) < frames) {
                atomic_store_explicit(&gInPrimed, 0, memory_order_relaxed); /* manque : réamorçage */
            }
        } else {
            memset(main, 0, (size_t)frames * gChannelsIn * sizeof(float));
        }
    } else if (op == kAudioServerPlugInIOOperationWriteMix && is_stream_out(stream) && main && region) {
        lw_ring_write(region, LW_TO_NET, (const float *)main, frames);
    }
    return kAudioHardwareNoError;
}

static OSStatus LW_EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID id, UInt32 client, UInt32 op,
                                  UInt32 frames, const AudioServerPlugInIOCycleInfo *info) {
    (void)client, (void)op, (void)frames, (void)info;
    return (d == gDriverRef && dev_index(id) >= 0) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
