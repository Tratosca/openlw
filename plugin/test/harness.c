/*
 * Banc d'essai du plugin HAL, sans installation ni coreaudiod.
 *
 * Joue deux rôles dans un même processus :
 *  - « coreaudiod » : charge le bundle par CFPlugIn (fabrique déclarée dans Info.plist), obtient
 *    l'interface AudioServerPlugInDriverInterface, interroge les propriétés, lance l'IO et simule des
 *    cycles ReadInput / WriteMix ;
 *  - « daemon » : crée la région partagée (lw_shm) et un service XPC anonyme qui la remet au plugin.
 *
 * Usage : harness build/OpenLW.driver     (code de sortie ≠ 0 en cas d'échec)
 */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "lw_shm.h"
#include "lw_sys.h"

static int gFailures = 0;
#define CHECK(cond, ...)                                                                                               \
    do {                                                                                                               \
        if (cond) {                                                                                                    \
            printf("  ok   ");                                                                                         \
        } else {                                                                                                       \
            printf("  ÉCHEC ");                                                                                        \
            gFailures++;                                                                                               \
        }                                                                                                              \
        printf(__VA_ARGS__);                                                                                           \
        printf("\n");                                                                                                  \
    } while (0)

/* ---------- Hôte simulé ---------- */

static UInt32 gPropertyChanges = 0;
static OSStatus HostPropertiesChanged(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n,
                                      const AudioObjectPropertyAddress *a) {
    (void)h, (void)o, (void)a;
    gPropertyChanges += n;
    return 0;
}
static OSStatus HostCopyFromStorage(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef *out) {
    (void)h, (void)k;
    *out = NULL;
    return 0;
}
static OSStatus HostWriteToStorage(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef d) {
    (void)h, (void)k, (void)d;
    return 0;
}
static OSStatus HostDeleteFromStorage(AudioServerPlugInHostRef h, CFStringRef k) {
    (void)h, (void)k;
    return 0;
}
static UInt32 gConfigRequests = 0;
static OSStatus HostRequestConfigChange(AudioServerPlugInHostRef h, AudioObjectID o, UInt64 a, void *i) {
    (void)h, (void)o, (void)a, (void)i;
    gConfigRequests++;
    return 0;
}
static AudioServerPlugInHostInterface gHostInterface = {HostPropertiesChanged, HostCopyFromStorage, HostWriteToStorage,
                                                        HostDeleteFromStorage, HostRequestConfigChange};

/* ---------- « Daemon » simulé ---------- */

/* Géométrie annoncée par le daemon simulé (commande "geometry", et génération dans "attach"). */
static unsigned long long gGen = 1;
static unsigned gTo = 2, gFrom = 2;
/* Fragment JSON des noms ajouté à la réponse "geometry" (vide : pas de noms). */
static const char *gNames = "";

static char *handler(const char *req, uint32_t uid, void *ctx) {
    (void)uid, (void)ctx;
    char buf[1024];
    if (strstr(req, "geometry")) {
        snprintf(buf, sizeof buf, "{\"ok\":true,\"generation\":%llu,\"channels_to_net\":%u,\"channels_from_net\":%u%s}",
                 gGen, gTo, gFrom, gNames);
    } else {
        snprintf(buf, sizeof buf, "{\"ok\":true,\"device\":{\"generation\":%llu}}", gGen);
    }
    return strdup(buf);
}
static void free_resp(char *p) {
    free(p);
}

/* ---------- Aides ---------- */

static AudioServerPlugInDriverInterface **gDrv;
#define DRV (*gDrv)

static UInt32 get_u32(AudioObjectID obj, AudioObjectPropertySelector sel, AudioObjectPropertyScope scope) {
    AudioObjectPropertyAddress a = {sel, scope, 0};
    UInt32 v = 0xFFFFFFFF, size = 0;
    DRV->GetPropertyData(gDrv, obj, getpid(), &a, 0, NULL, sizeof v, &size, &v);
    return v;
}

static int get_str(AudioObjectID obj, AudioObjectPropertySelector sel, char *buf, size_t n) {
    AudioObjectPropertyAddress a = {sel, kAudioObjectPropertyScopeGlobal, 0};
    CFStringRef s = NULL;
    UInt32 size = 0;
    if (DRV->GetPropertyData(gDrv, obj, getpid(), &a, 0, NULL, sizeof s, &size, &s) != 0 || s == NULL) {
        return -1;
    }
    CFStringGetCString(s, buf, (CFIndex)n, kCFStringEncodingUTF8);
    CFRelease(s);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage : %s <bundle.driver>\n", argv[0]);
        return 2;
    }
    printf("Chargement CFPlugIn de %s\n", argv[1]);
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)argv[1], (CFIndex)strlen(argv[1]), true);
    CFPlugInRef plugin = CFPlugInCreate(NULL, url);
    CHECK(plugin != NULL, "bundle chargé");
    if (plugin == NULL) {
        return 1;
    }
    CFArrayRef factories = CFPlugInFindFactoriesForPlugInType(kAudioServerPlugInTypeUUID);
    CHECK(factories && CFArrayGetCount(factories) >= 1, "fabrique déclarée pour kAudioServerPlugInTypeUUID");
    if (!factories || CFArrayGetCount(factories) < 1) {
        return 1;
    }
    CFUUIDRef factory = CFArrayGetValueAtIndex(factories, 0);
    void *iunknown = CFPlugInInstanceCreate(NULL, factory, kAudioServerPlugInTypeUUID);
    CHECK(iunknown != NULL, "instance créée par la fabrique LW_Create");
    IUnknownVTbl **unk = (IUnknownVTbl **)iunknown;
    void *drv = NULL;
    HRESULT hr = (*unk)->QueryInterface(unk, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &drv);
    CHECK(hr == S_OK && drv != NULL, "QueryInterface(kAudioServerPlugInDriverInterfaceUUID)");
    gDrv = drv;

    CHECK(DRV->Initialize(gDrv, &gHostInterface) == 0, "Initialize");

    printf("Propriétés\n");
    AudioObjectPropertyAddress a = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, 0};
    AudioObjectID devs[4] = {0};
    UInt32 size = 0;
    DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &a, 0, NULL, sizeof devs, &size, devs);
    CHECK(size == sizeof(AudioObjectID) && devs[0] == 2, "un périphérique (id %u)", devs[0]);
    char name[128];
    CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW") == 0,
          "nom « %s »", name);
    CHECK(get_str(2, kAudioDevicePropertyDeviceUID, name, sizeof name) == 0 &&
              strcmp(name, "fr.francois-brille.openlw.device") == 0,
          "UID « %s »", name);
    CFStringRef uid = CFSTR("fr.francois-brille.openlw.device");
    AudioObjectID found = 0;
    a.mSelector = kAudioPlugInPropertyTranslateUIDToDevice;
    DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &a, sizeof uid, &uid, sizeof found, &size, &found);
    CHECK(found == 2, "TranslateUIDToDevice");
    CHECK(get_u32(2, kAudioObjectPropertyClass, kAudioObjectPropertyScopeGlobal) == kAudioDeviceClassID, "classe périphérique");
    CHECK(get_u32(2, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal) == kAudioDeviceTransportTypeVirtual,
          "transport virtuel");
    CHECK(get_u32(2, kAudioDevicePropertyDeviceCanBeDefaultDevice, kAudioObjectPropertyScopeGlobal) == 1,
          "peut être le périphérique par défaut");
    Float64 rate = 0;
    a = (AudioObjectPropertyAddress){kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, 0};
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof rate, &size, &rate);
    CHECK(rate == 48000.0, "fréquence nominale %.0f Hz", rate);
    AudioObjectID in = 0, out = 0;
    a = (AudioObjectPropertyAddress){kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, 0};
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof in, &size, &in);
    a.mScope = kAudioObjectPropertyScopeOutput;
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof out, &size, &out);
    CHECK(in == 3 && out == 4, "flux d'entrée %u, de sortie %u", in, out);
    CHECK(get_u32(3, kAudioStreamPropertyDirection, kAudioObjectPropertyScopeGlobal) == 1 &&
              get_u32(4, kAudioStreamPropertyDirection, kAudioObjectPropertyScopeGlobal) == 0,
          "sens des flux");
    AudioStreamBasicDescription f;
    a = (AudioObjectPropertyAddress){kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
    DRV->GetPropertyData(gDrv, 4, getpid(), &a, 0, NULL, sizeof f, &size, &f);
    UInt32 ch = f.mChannelsPerFrame;
    CHECK(f.mFormatID == kAudioFormatLinearPCM && f.mBitsPerChannel == 32 && (f.mFormatFlags & kAudioFormatFlagIsFloat) &&
              f.mSampleRate == 48000.0 && ch == 2,
          "format float32 48 kHz, %u canaux", ch);
    Boolean settable = true;
    a.mSelector = kAudioDevicePropertyNominalSampleRate;
    DRV->IsPropertySettable(gDrv, 2, getpid(), &a, &settable);
    CHECK(!settable, "fréquence non modifiable");
    a.mSelector = 'zzzz';
    CHECK(!DRV->HasProperty(gDrv, 2, getpid(), &a), "propriété inconnue refusée");

    AudioServerPlugInIOCycleInfo cycle_info;
    memset(&cycle_info, 0, sizeof cycle_info);

    printf("IO sans daemon\n");
    /* Service simulé qui ne fournit aucune région (le vrai daemon peut tourner sur la machine de test). */
    void (*use_endpoint)(void *) =
        CFBundleGetFunctionPointerForName(CFPlugInGetBundle(plugin), CFSTR("lw_plugin_test_use_endpoint"));
    CHECK(use_endpoint != NULL, "hook de test exporté");
    lw_server *empty = lw_xpc_server_start(NULL, handler, free_resp, NULL);
    void *empty_endpoint = lw_xpc_server_endpoint(empty);
    use_endpoint(empty_endpoint);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (daemon absent)");
    float *buf = calloc(512 * ch, sizeof(float));
    for (UInt32 i = 0; i < 512 * ch; i++) {
        buf[i] = 1.0f;
    }
    DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, buf, NULL);
    int silent = 1;
    for (UInt32 i = 0; i < 512 * ch; i++) {
        silent &= buf[i] == 0.0f;
    }
    CHECK(silent, "entrée silencieuse sans daemon");
    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("IO avec daemon simulé (région partagée par XPC)\n");
    size_t rsize = lw_shm_size(8192, ch, ch);
    void *shmem = NULL;
    void *region = lw_shm_alloc(rsize, &shmem);
    CHECK(region && lw_shm_init(region, rsize, 48000, 8192, ch, ch) == 0, "région créée (%zu octets)", rsize);
    lw_server *server = lw_xpc_server_start(NULL, handler, free_resp, NULL);
    lw_xpc_server_set_shmem(server, shmem);
    void *endpoint = lw_xpc_server_endpoint(server);
    use_endpoint(endpoint);
    lw_xpc_release(empty_endpoint);
    lw_xpc_server_stop(empty);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (attachement)");
    CHECK(get_u32(2, kAudioDevicePropertyDeviceIsRunning, kAudioObjectPropertyScopeGlobal) == 1, "périphérique en marche");

    /* Réseau → applications : le daemon écrit, le plugin restitue en ReadInput. Le daemon garde
     * 256 trames d'avance (marge d'amorçage du plugin) : le bloc lu est celui écrit 256 trames plus tôt. */
    float *src = calloc(512 * ch, sizeof(float));
    float *ahead = calloc(512 * ch, sizeof(float));
#define SIG(n, c) ((float)sin(0.01 * (double)(n)) * (float)((c) + 1) / 8.0f)
    for (UInt32 i = 0; i < 256; i++) {
        for (UInt32 c = 0; c < ch; c++) {
            ahead[i * ch + c] = SIG(i, c);
        }
    }
    lw_ring_write(region, LW_FROM_NET, ahead, 256);
    int same_in = 1, same_out = 1;
    for (int cycle = 0; cycle < 100; cycle++) {
        for (UInt32 i = 0; i < 512; i++) {
            for (UInt32 c = 0; c < ch; c++) {
                ahead[i * ch + c] = SIG(256 + cycle * 512 + (int)i, c);
                src[i * ch + c] = SIG(cycle * 512 + (int)i, c);
            }
        }
        lw_ring_write(region, LW_FROM_NET, ahead, 512);
        DRV->BeginIOOperation(gDrv, 2, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info);
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, buf, NULL);
        DRV->EndIOOperation(gDrv, 2, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info);
        same_in &= memcmp(buf, src, 512 * ch * sizeof(float)) == 0;
        /* Applications → réseau : le plugin reçoit le mixage en WriteMix, le daemon le relit. */
        for (UInt32 i = 0; i < 512 * ch; i++) {
            src[i] = -src[i];
        }
        DRV->DoIOOperation(gDrv, 2, 4, 1, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, src, NULL);
        lw_ring_read(region, LW_TO_NET, buf, 512);
        same_out &= memcmp(buf, src, 512 * ch * sizeof(float)) == 0;
    }
    CHECK(same_in, "réseau → applications : 100 cycles de 512 trames identiques");
    CHECK(same_out, "applications → réseau : 100 cycles de 512 trames identiques");
    uint64_t w, r, ov, un;
    lw_ring_counters(region, LW_FROM_NET, &w, &r, &ov, &un);
    CHECK(ov == 0 && un == 0 && w == 51456, "compteurs sans perte (%llu trames)", (unsigned long long)w);

    printf("Horodatage zéro\n");
    Float64 st0, st1;
    UInt64 ht0, ht1, seed;
    DRV->GetZeroTimeStamp(gDrv, 2, 1, &st0, &ht0, &seed);
    usleep(400000); /* > 16384 trames à 48 kHz */
    DRV->GetZeroTimeStamp(gDrv, 2, 1, &st1, &ht1, &seed);
    CHECK(st1 == st0 + 16384.0 && ht1 > ht0, "période 16384 trames, temps hôte croissant (%.0f → %.0f)", st0, st1);
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    double period_s = (double)(ht1 - ht0) * tb.numer / tb.denom / 1e9;
    CHECK(fabs(period_s - 16384.0 / 48000.0) < 1e-6, "durée d'une période %.6f s (attendu %.6f)", period_s,
          16384.0 / 48000.0);

    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("Nombre de canaux modifié par le daemon\n");
    void (*poll)(void) = CFBundleGetFunctionPointerForName(CFPlugInGetBundle(plugin), CFSTR("lw_plugin_test_poll"));
    CHECK(poll != NULL, "hook de surveillance exporté");
    gConfigRequests = 0;
    poll();
    CHECK(gConfigRequests == 0, "géométrie inchangée : aucune demande à l'hôte");
    gGen = 2, gTo = 4, gFrom = 6;
    poll();
    poll();
    CHECK(gConfigRequests == 1, "nouvelle géométrie : une demande de changement de configuration (%u)", gConfigRequests);
    CHECK(DRV->PerformDeviceConfigurationChange(gDrv, 2, 0, NULL) == 0, "changement appliqué par l'hôte");
    a = (AudioObjectPropertyAddress){kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
    AudioStreamBasicDescription fin, fout;
    DRV->GetPropertyData(gDrv, 3, getpid(), &a, 0, NULL, sizeof fin, &size, &fin);
    DRV->GetPropertyData(gDrv, 4, getpid(), &a, 0, NULL, sizeof fout, &size, &fout);
    CHECK(fout.mChannelsPerFrame == 4 && fin.mChannelsPerFrame == 6, "formats : %u sorties, %u entrées",
          fout.mChannelsPerFrame, fin.mChannelsPerFrame);
    size_t rsize2 = lw_shm_size(8192, 4, 6);
    void *shmem2 = NULL;
    void *region2 = lw_shm_alloc(rsize2, &shmem2);
    CHECK(region2 && lw_shm_init(region2, rsize2, 48000, 8192, 4, 6) == 0, "région n° 2 créée (4 x 6)");
    lw_xpc_server_set_shmem(server, shmem2);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (rattachement)");
    float six[512 * 6], back[512 * 6];
    for (UInt32 i = 0; i < 512 * 6; i++) {
        six[i] = (float)i / 4096.0f;
    }
    lw_ring_write(region2, LW_FROM_NET, six, 512);
    lw_ring_write(region2, LW_FROM_NET, six, 256); /* marge d'amorçage */
    DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, back, NULL);
    CHECK(memcmp(six, back, sizeof six) == 0, "6 entrées restituées depuis la nouvelle région");
    poll();
    CHECK(gConfigRequests == 1, "région à jour : plus de demande");
    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("Noms du périphérique et des canaux\n");
    {
        UInt32 before = gPropertyChanges;
        gNames = ",\"name\":\"OpenLW (2 - Studio A)\",\"input_names\":[\"2 - Studio A G\",\"2 - Studio A D\",\"\",\"\","
                 "\"Régie \\\"A\\\" G\",\"\"],\"output_names\":[\"4005 - STUDIO MAC G\",\"4005 - STUDIO MAC D\",\"\",\"\"]";
        poll();
        char name[128];
        CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW (2 - Studio A)") == 0,
              "nom du périphérique : %s", name);
        CHECK(gPropertyChanges > before, "changement de nom signalé à l'hôte");
        AudioObjectPropertyAddress ea = {kAudioObjectPropertyElementName, kAudioObjectPropertyScopeInput, 1};
        CFStringRef s = NULL;
        UInt32 sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "2 - Studio A G") == 0,
              "entrée 1 : %s", name);
        if (s) CFRelease(s);
        ea.mElement = 5;
        sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "Régie \"A\" G") == 0,
              "entrée 5 (accent et guillemets échappés) : %s", name);
        if (s) CFRelease(s);
        ea.mScope = kAudioObjectPropertyScopeOutput;
        ea.mElement = 2;
        sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "4005 - STUDIO MAC D") == 0,
              "sortie 2 : %s", name);
        if (s) CFRelease(s);
        ea.mElement = 9;
        CHECK(!DRV->HasProperty(gDrv, 2, getpid(), &ea), "sortie 9 inexistante : pas de nom");
        before = gPropertyChanges;
        poll();
        CHECK(gPropertyChanges == before, "noms inchangés : pas de notification");
        gNames = "";
        poll();
        CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW") == 0,
              "sans noms du daemon : « %s »", name);
    }

    printf("Marge d'entrée fixée par le daemon\n");
    {
        enum { C = 6 };
        static float few[1024 * C], got[512 * C];
        gNames = ",\"input_margin\":128";
        poll();
        CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO");
        for (int i = 0; i < 640 * C; i++) {
            few[i] = 0.75f;
        }
        lw_ring_write(region2, LW_FROM_NET, few, 639); /* 512 + 127 : sous la marge de 128 */
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got, NULL);
        CHECK(got[0] == 0.0f, "512 + 127 trames : pas encore amorcé");
        lw_ring_write(region2, LW_FROM_NET, few, 1);
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got, NULL);
        CHECK(got[0] == 0.75f, "512 + 128 trames : amorcé avec la marge du préréglage faible");
        CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");
        gNames = ",\"input_margin\":256";
        poll();
        gNames = "";
    }

    printf("Entrée par blocs de 4096 trames (cas d'Audacity)\n");
    {
        enum { B = 4096, C = 6 };
        static float blk[B * C], wr[48 * C];
        /* Audio ancien laissé par une IO arrêtée : 7000 trames de valeur -1. */
        for (int i = 0; i < 7000; i++) {
            for (int c = 0; c < C; c++) {
                wr[c] = -1.0f;
            }
            lw_ring_write(region2, LW_FROM_NET, wr, 1);
        }
        CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO");
        /* Le daemon écrit 48 trames par ms (rampe sur le canal 1) ; l'hôte lit un bloc quand il est dû. */
        uint64_t written = 0, reads = 0, silent_blocks = 0, stale = 0, breaks = 0;
        float expect = -1.0f;
        for (int ms = 0; ms < 3000; ms++) {
            for (int i = 0; i < 48; i++) {
                for (int c = 0; c < C; c++) {
                    wr[i * C + c] = c == 0 ? (float)(written + (uint64_t)i) : 0.5f;
                }
            }
            /* Lecture du bloc pendant l'écriture : comme l'hôte, sans attendre une frontière de tick. */
            lw_ring_write(region2, LW_FROM_NET, wr, 24);
            if (written + 24 >= 4400 + reads * B) {
                DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
                int all_zero = 1;
                for (int i = 0; i < B; i++) {
                    float v = blk[i * C];
                    all_zero &= v == 0.0f;
                    stale += v == -1.0f;
                    if (expect >= 0 && v != expect) {
                        breaks++;
                    }
                    expect = v + 1.0f;
                }
                silent_blocks += all_zero;
                reads++;
            }
            lw_ring_write(region2, LW_FROM_NET, wr + 24 * C, 24);
            written += 48;
        }
        CHECK(reads >= 34 && silent_blocks == 0, "%llu blocs lus, aucun bloc silencieux", (unsigned long long)reads);
        CHECK(stale == 0, "audio ancien jeté au démarrage (%llu trames anciennes lues)", (unsigned long long)stale);
        CHECK(breaks == 0, "rampe continue d'un bloc à l'autre (%llu ruptures)", (unsigned long long)breaks);
        /* Manque : le daemon s'arrête 200 ms ; silence, puis reprise propre. */
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        int zero = 1;
        for (int i = 0; i < B * C; i++) {
            zero &= blk[i] == 0.0f;
        }
        CHECK(zero, "manque de données : bloc silencieux");
        for (int i = 0; i < B + 300; i++) {
            for (int c = 0; c < C; c++) {
                wr[c] = 0.25f;
            }
            lw_ring_write(region2, LW_FROM_NET, wr, 1);
        }
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        CHECK(blk[0] == 0.25f && blk[B * C - 1] == 0.25f, "réamorçage après le manque");
        CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");
    }
    printf("Deux périphériques (OpenLW In / OpenLW Out)\n");
    {
        UInt32 before = gPropertyChanges;
        gNames = ",\"layout\":\"split\",\"input_device_name\":\"OpenLW In (2 - Studio A)\"";
        poll();
        AudioObjectPropertyAddress la = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, 0};
        AudioObjectID devs[4] = {0};
        UInt32 sz = sizeof devs;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &la, 0, NULL, sz, &sz, devs);
        CHECK(sz == 2 * sizeof(AudioObjectID) && devs[0] == 5 && devs[1] == 7, "liste : %u périphérique(s), %u et %u",
              (unsigned)(sz / sizeof(AudioObjectID)), devs[0], devs[1]);
        CHECK(gPropertyChanges > before, "changement de la liste signalé à l'hôte");
        char name[128];
        CHECK(get_str(5, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW In (2 - Studio A)") == 0,
              "entrée : %s", name);
        CHECK(get_str(7, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW Out") == 0,
              "sortie : %s", name);
        CHECK(get_str(5, kAudioDevicePropertyDeviceUID, name, sizeof name) == 0 &&
                  strcmp(name, "fr.francois-brille.openlw.device.in") == 0,
              "UID : %s", name);
        AudioObjectPropertyAddress sa = {kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, 0};
        AudioObjectID st[2] = {0};
        sz = sizeof st;
        DRV->GetPropertyData(gDrv, 5, getpid(), &sa, 0, NULL, sz, &sz, st);
        UInt32 in_n = sz / sizeof(AudioObjectID);
        AudioObjectID in_stream = st[0];
        sa.mScope = kAudioObjectPropertyScopeOutput;
        sz = sizeof st;
        DRV->GetPropertyData(gDrv, 5, getpid(), &sa, 0, NULL, sz, &sz, st);
        CHECK(in_n == 1 && in_stream == 6 && sz == 0, "OpenLW In : un flux d'entrée (6), aucune sortie");
        sz = sizeof st;
        DRV->GetPropertyData(gDrv, 7, getpid(), &sa, 0, NULL, sz, &sz, st);
        CHECK(sz == sizeof(AudioObjectID) && st[0] == 8, "OpenLW Out : un flux de sortie (8)");
        AudioObjectPropertyAddress fa = {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
        AudioStreamBasicDescription f6, f8;
        sz = sizeof f6;
        DRV->GetPropertyData(gDrv, 6, getpid(), &fa, 0, NULL, sz, &sz, &f6);
        sz = sizeof f8;
        DRV->GetPropertyData(gDrv, 8, getpid(), &fa, 0, NULL, sz, &sz, &f8);
        CHECK(f6.mChannelsPerFrame == 6 && f8.mChannelsPerFrame == 4, "formats : %u entrées, %u sorties",
              f6.mChannelsPerFrame, f8.mChannelsPerFrame);

        /* IO séparées : lecture sur OpenLW In, écriture sur OpenLW Out. */
        CHECK(DRV->StartIO(gDrv, 5, 1) == 0 && DRV->StartIO(gDrv, 7, 2) == 0, "StartIO des deux périphériques");
        static float six[1024 * 6], back[512 * 6], four[512 * 4], got4[512 * 4];
        for (int i = 0; i < 1024 * 6; i++) {
            six[i] = 0.5f;
        }
        lw_ring_write(region2, LW_FROM_NET, six, 1024);
        DRV->DoIOOperation(gDrv, 5, 6, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, back, NULL);
        CHECK(back[0] == 0.5f && back[512 * 6 - 1] == 0.5f, "OpenLW In restitue l'audio du réseau");
        for (int i = 0; i < 512 * 4; i++) {
            four[i] = 0.25f;
        }
        DRV->DoIOOperation(gDrv, 7, 8, 2, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, four, NULL);
        lw_ring_read(region2, LW_TO_NET, got4, 512);
        CHECK(got4[0] == 0.25f && got4[512 * 4 - 1] == 0.25f, "OpenLW Out envoie vers le réseau");
        CHECK(DRV->DoIOOperation(gDrv, 5, 8, 1, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, four, NULL) != 0,
              "flux d'un autre périphérique refusé");

        /* Nombre de canaux modifié : une demande par périphérique, appliquée sens par sens. */
        UInt32 req = gConfigRequests;
        gGen = 3, gTo = 2, gFrom = 2;
        poll();
        CHECK(gConfigRequests == req + 2, "deux demandes de changement de configuration (%u)", gConfigRequests - req);
        DRV->StopIO(gDrv, 5, 1);
        CHECK(DRV->PerformDeviceConfigurationChange(gDrv, 5, 0, NULL) == 0, "changement appliqué à OpenLW In");
        sz = sizeof f6;
        DRV->GetPropertyData(gDrv, 6, getpid(), &fa, 0, NULL, sz, &sz, &f6);
        sz = sizeof f8;
        DRV->GetPropertyData(gDrv, 8, getpid(), &fa, 0, NULL, sz, &sz, &f8);
        CHECK(f6.mChannelsPerFrame == 2 && f8.mChannelsPerFrame == 4, "entrées à 2, sorties encore à 4 (Out en marche)");
        DRV->StopIO(gDrv, 7, 2);
        DRV->PerformDeviceConfigurationChange(gDrv, 7, 0, NULL);
        sz = sizeof f8;
        DRV->GetPropertyData(gDrv, 8, getpid(), &fa, 0, NULL, sz, &sz, &f8);
        CHECK(f8.mChannelsPerFrame == 2, "sorties à 2 après OpenLW Out");

        gNames = "";
        poll();
        sz = sizeof devs;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &la, 0, NULL, sz, &sz, devs);
        CHECK(sz == sizeof(AudioObjectID) && devs[0] == 2, "retour à un périphérique duplex");
    }

    use_endpoint(NULL);
    lw_xpc_release(endpoint);
    lw_xpc_server_stop(server);
    lw_xpc_release(shmem);
    lw_shm_unmap(region, rsize);
    lw_xpc_release(shmem2);
    lw_shm_unmap(region2, rsize2);
    free(buf);
    free(src);
    free(ahead);

    printf("%s : %d échec(s)\n", gFailures ? "ÉCHEC" : "SUCCÈS", gFailures);
    return gFailures ? 1 : 0;
}
