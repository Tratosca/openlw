/*
 * HAL plugin harness, without installation or coreaudiod.
 *
 * Acts in two roles within one process:
 *  - “coreaudiod”: loads bundle through CFPlugIn (factory in Info.plist), obtains
 * AudioServerPlugInDriverInterface, queries properties, starts I/O, and simulates
 * ReadInput / WriteMix cycles;
 *  - “daemon”: creates shared region (lw_shm) and anonymous XPC service providing it to plugin.
 *
 * Usage: harness build/OpenLW.driver     (nonzero exit code on failure)
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
            printf("  FAIL ");                                                                                         \
            gFailures++;                                                                                               \
        }                                                                                                              \
        printf(__VA_ARGS__);                                                                                           \
        printf("\n");                                                                                                  \
    } while (0)

/* ---------- Simulated host ---------- */

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

/* ---------- Simulated “daemon” ---------- */

/* Geometry advertised by simulated daemon ("geometry" command, generation in "attach"). */
static unsigned long long gGen = 1;
static unsigned gTo = 2, gFrom = 2;
/* Names JSON fragment added to "geometry" response (empty: no names). */
static const char *gNames = "";

static char *handler(const char *req, uint32_t uid, void *ctx) {
    (void)uid, (void)ctx;
    char buf[4096];
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

/* ---------- Helpers ---------- */

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
        fprintf(stderr, "usage: %s <bundle.driver>\n", argv[0]);
        return 2;
    }
    printf("CFPlugIn loading of %s\n", argv[1]);
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)argv[1], (CFIndex)strlen(argv[1]), true);
    CFPlugInRef plugin = CFPlugInCreate(NULL, url);
    CHECK(plugin != NULL, "bundle loaded");
    if (plugin == NULL) {
        return 1;
    }
    CFArrayRef factories = CFPlugInFindFactoriesForPlugInType(kAudioServerPlugInTypeUUID);
    CHECK(factories && CFArrayGetCount(factories) >= 1, "factory declared for kAudioServerPlugInTypeUUID");
    if (!factories || CFArrayGetCount(factories) < 1) {
        return 1;
    }
    CFUUIDRef factory = CFArrayGetValueAtIndex(factories, 0);
    void *iunknown = CFPlugInInstanceCreate(NULL, factory, kAudioServerPlugInTypeUUID);
    CHECK(iunknown != NULL, "instance created by the LW_Create factory");
    IUnknownVTbl **unk = (IUnknownVTbl **)iunknown;
    void *drv = NULL;
    HRESULT hr = (*unk)->QueryInterface(unk, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &drv);
    CHECK(hr == S_OK && drv != NULL, "QueryInterface(kAudioServerPlugInDriverInterfaceUUID)");
    gDrv = drv;

    CHECK(DRV->Initialize(gDrv, &gHostInterface) == 0, "Initialize");

    printf("Properties\n");
    AudioObjectPropertyAddress a = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, 0};
    AudioObjectID devs[4] = {0};
    UInt32 size = 0;
    DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &a, 0, NULL, sizeof devs, &size, devs);
    CHECK(size == sizeof(AudioObjectID) && devs[0] == 2, "one device (id %u)", devs[0]);
    char name[128];
    CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW") == 0,
          "name “%s”", name);
    CHECK(get_str(2, kAudioDevicePropertyDeviceUID, name, sizeof name) == 0 &&
              strcmp(name, "fr.francois-brille.openlw.device") == 0,
          "UID “%s”", name);
    CFStringRef uid = CFSTR("fr.francois-brille.openlw.device");
    AudioObjectID found = 0;
    a.mSelector = kAudioPlugInPropertyTranslateUIDToDevice;
    DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &a, sizeof uid, &uid, sizeof found, &size, &found);
    CHECK(found == 2, "TranslateUIDToDevice");
    CHECK(get_u32(2, kAudioObjectPropertyClass, kAudioObjectPropertyScopeGlobal) == kAudioDeviceClassID, "device class");
    CHECK(get_u32(2, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal) == kAudioDeviceTransportTypeVirtual,
          "virtual transport");
    CHECK(get_u32(2, kAudioDevicePropertyDeviceCanBeDefaultDevice, kAudioObjectPropertyScopeGlobal) == 1,
          "can be the default device");
    Float64 rate = 0;
    a = (AudioObjectPropertyAddress){kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, 0};
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof rate, &size, &rate);
    CHECK(rate == 48000.0, "nominal sample rate %.0f Hz", rate);
    AudioObjectID in = 0, out = 0;
    a = (AudioObjectPropertyAddress){kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, 0};
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof in, &size, &in);
    a.mScope = kAudioObjectPropertyScopeOutput;
    DRV->GetPropertyData(gDrv, 2, getpid(), &a, 0, NULL, sizeof out, &size, &out);
    CHECK(in == 3 && out == 4, "input stream %u, output stream %u", in, out);
    CHECK(get_u32(3, kAudioStreamPropertyDirection, kAudioObjectPropertyScopeGlobal) == 1 &&
              get_u32(4, kAudioStreamPropertyDirection, kAudioObjectPropertyScopeGlobal) == 0,
          "stream directions");
    AudioStreamBasicDescription f;
    a = (AudioObjectPropertyAddress){kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
    DRV->GetPropertyData(gDrv, 4, getpid(), &a, 0, NULL, sizeof f, &size, &f);
    UInt32 ch = f.mChannelsPerFrame;
    CHECK(f.mFormatID == kAudioFormatLinearPCM && f.mBitsPerChannel == 32 && (f.mFormatFlags & kAudioFormatFlagIsFloat) &&
              f.mSampleRate == 48000.0 && ch == 2,
          "format float32 48 kHz, %u channels", ch);
    Boolean settable = true;
    a.mSelector = kAudioDevicePropertyNominalSampleRate;
    DRV->IsPropertySettable(gDrv, 2, getpid(), &a, &settable);
    CHECK(!settable, "sample rate not settable");
    a.mSelector = 'zzzz';
    CHECK(!DRV->HasProperty(gDrv, 2, getpid(), &a), "unknown property rejected");

    AudioServerPlugInIOCycleInfo cycle_info;
    memset(&cycle_info, 0, sizeof cycle_info);

    printf("I/O without daemon\n");
    /* Mock service providing no region (real daemon may run on test machine). */
    void (*use_endpoint)(void *) =
        CFBundleGetFunctionPointerForName(CFPlugInGetBundle(plugin), CFSTR("lw_plugin_test_use_endpoint"));
    CHECK(use_endpoint != NULL, "test hook exported");
    lw_server *empty = lw_xpc_server_start(NULL, handler, free_resp, NULL);
    void *empty_endpoint = lw_xpc_server_endpoint(empty);
    use_endpoint(empty_endpoint);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (no daemon)");
    float *buf = calloc(512 * ch, sizeof(float));
    for (UInt32 i = 0; i < 512 * ch; i++) {
        buf[i] = 1.0f;
    }
    DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, buf, NULL);
    int silent = 1;
    for (UInt32 i = 0; i < 512 * ch; i++) {
        silent &= buf[i] == 0.0f;
    }
    CHECK(silent, "silent input without daemon");
    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("I/O with simulated daemon (shared region over XPC)\n");
    lw_ring_spec spec[2] = {{LW_TO_NET, ch}, {LW_FROM_NET, ch}};
    size_t rsize = lw_shm_size(8192, 2, spec);
    lw_host_clock clock;
    lw_host_clock_info(&clock);
    void *shmem = NULL;
    void *region = lw_shm_alloc(rsize, &shmem);
    CHECK(region && lw_shm_init(region, rsize, 48000, 8192, 2, spec, &clock) == 0, "region created (%zu bytes)", rsize);
    lw_server *server = lw_xpc_server_start(NULL, handler, free_resp, NULL);
    lw_xpc_server_set_shmem(server, shmem);
    void *endpoint = lw_xpc_server_endpoint(server);
    use_endpoint(endpoint);
    lw_xpc_release(empty_endpoint);
    lw_xpc_server_stop(empty);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (attach)");
    CHECK(get_u32(2, kAudioDevicePropertyDeviceIsRunning, kAudioObjectPropertyScopeGlobal) == 1, "device running");

    /* Network → applications: daemon writes, plugin returns in ReadInput. Daemon maintains
     * 256-frame lead (plugin priming margin): read block was written 256 frames earlier. */
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
        /* Applications → network: plugin receives mix in WriteMix; daemon reads it back. */
        for (UInt32 i = 0; i < 512 * ch; i++) {
            src[i] = -src[i];
        }
        DRV->DoIOOperation(gDrv, 2, 4, 1, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, src, NULL);
        lw_ring_read(region, LW_TO_NET, buf, 512);
        same_out &= memcmp(buf, src, 512 * ch * sizeof(float)) == 0;
    }
    CHECK(same_in, "network → applications: 100 cycles of 512 identical frames");
    CHECK(same_out, "applications → network: 100 cycles of 512 identical frames");
    uint64_t w, r, ov, un;
    lw_ring_counters(region, LW_FROM_NET, &w, &r, &ov, &un);
    CHECK(ov == 0 && un == 0 && w == 51456, "counters without loss (%llu frames)", (unsigned long long)w);

    printf("Zero timestamp\n");
    Float64 st0, st1;
    UInt64 ht0, ht1, seed;
    DRV->GetZeroTimeStamp(gDrv, 2, 1, &st0, &ht0, &seed);
    usleep(400000); /* > 16384 frames at 48 kHz */
    DRV->GetZeroTimeStamp(gDrv, 2, 1, &st1, &ht1, &seed);
    CHECK(st1 == st0 + 16384.0 && ht1 > ht0, "period 16384 frames, host time increasing (%.0f → %.0f)", st0, st1);
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    double period_s = (double)(ht1 - ht0) * tb.numer / tb.denom / 1e9;
    CHECK(fabs(period_s - 16384.0 / 48000.0) < 1e-6, "period duration %.6f s (expected %.6f)", period_s,
          16384.0 / 48000.0);

    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("Channel count changed by the daemon\n");
    void (*poll)(void) = CFBundleGetFunctionPointerForName(CFPlugInGetBundle(plugin), CFSTR("lw_plugin_test_poll"));
    CHECK(poll != NULL, "monitoring hook exported");
    gConfigRequests = 0;
    poll();
    CHECK(gConfigRequests == 0, "geometry unchanged: no request to the host");
    gGen = 2, gTo = 4, gFrom = 6;
    poll();
    poll();
    CHECK(gConfigRequests == 1, "new geometry: one configuration change request (%u)", gConfigRequests);
    CHECK(DRV->PerformDeviceConfigurationChange(gDrv, 2, 0, NULL) == 0, "change applied by the host");
    a = (AudioObjectPropertyAddress){kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
    AudioStreamBasicDescription fin, fout;
    DRV->GetPropertyData(gDrv, 3, getpid(), &a, 0, NULL, sizeof fin, &size, &fin);
    DRV->GetPropertyData(gDrv, 4, getpid(), &a, 0, NULL, sizeof fout, &size, &fout);
    CHECK(fout.mChannelsPerFrame == 4 && fin.mChannelsPerFrame == 6, "formats: %u outputs, %u inputs",
          fout.mChannelsPerFrame, fin.mChannelsPerFrame);
    lw_ring_spec spec2[2] = {{LW_TO_NET, 4}, {LW_FROM_NET, 6}};
    size_t rsize2 = lw_shm_size(8192, 2, spec2);
    void *shmem2 = NULL;
    void *region2 = lw_shm_alloc(rsize2, &shmem2);
    CHECK(region2 && lw_shm_init(region2, rsize2, 48000, 8192, 2, spec2, &clock) == 0, "region #2 created (4 x 6)");
    lw_xpc_server_set_shmem(server, shmem2);
    CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO (reattach)");
    float six[512 * 6], back[512 * 6];
    for (UInt32 i = 0; i < 512 * 6; i++) {
        six[i] = (float)i / 4096.0f;
    }
    lw_ring_write(region2, LW_FROM_NET, six, 512);
    lw_ring_write(region2, LW_FROM_NET, six, 256); /* Priming margin */
    DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, back, NULL);
    CHECK(memcmp(six, back, sizeof six) == 0, "6 inputs read back from the new region");
    poll();
    CHECK(gConfigRequests == 1, "region up to date: no further request");
    CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");

    printf("Device and channel names\n");
    {
        UInt32 before = gPropertyChanges;
        gNames = ",\"name\":\"OpenLW (2 - Studio A)\",\"input_names\":[\"2 - Studio A L\",\"2 - Studio A R\",\"\",\"\","
                 "\"Café \\\"A\\\" L\",\"\"],\"output_names\":[\"4005 - STUDIO MAC L\",\"4005 - STUDIO MAC R\",\"\",\"\"]";
        poll();
        char name[128];
        CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW (2 - Studio A)") == 0,
              "device name: %s", name);
        CHECK(gPropertyChanges > before, "name change reported to the host");
        AudioObjectPropertyAddress ea = {kAudioObjectPropertyElementName, kAudioObjectPropertyScopeInput, 1};
        CFStringRef s = NULL;
        UInt32 sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "2 - Studio A L") == 0,
              "input 1: %s", name);
        if (s) CFRelease(s);
        ea.mElement = 5;
        sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "Café \"A\" L") == 0,
              "input 5 (accent, escaped quotes): %s", name);
        if (s) CFRelease(s);
        ea.mScope = kAudioObjectPropertyScopeOutput;
        ea.mElement = 2;
        sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 2, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "4005 - STUDIO MAC R") == 0,
              "output 2: %s", name);
        if (s) CFRelease(s);
        ea.mElement = 9;
        CHECK(!DRV->HasProperty(gDrv, 2, getpid(), &ea), "output 9 does not exist: no name");
        before = gPropertyChanges;
        poll();
        CHECK(gPropertyChanges == before, "names unchanged: no notification");
        gNames = "";
        poll();
        CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW") == 0,
              "without names from the daemon: “%s”", name);
    }

    printf("Input margin set by the daemon\n");
    {
        enum { C = 6 };
        static float few[1024 * C], got[512 * C];
        gNames = ",\"input_margin\":128";
        poll();
        CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO");
        for (int i = 0; i < 640 * C; i++) {
            few[i] = 0.75f;
        }
        lw_ring_write(region2, LW_FROM_NET, few, 639); /* 512 + 127: below the 128-frame margin */
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got, NULL);
        CHECK(got[0] == 0.0f, "512 + 127 frames: not primed yet");
        lw_ring_write(region2, LW_FROM_NET, few, 1);
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got, NULL);
        CHECK(got[0] == 0.75f, "512 + 128 frames: primed with the low preset margin");
        CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");
        gNames = ",\"input_margin\":256";
        poll();
        gNames = "";
    }

    printf("Input in 4096-frame blocks (Audacity case)\n");
    {
        enum { B = 4096, C = 6 };
        static float blk[B * C], wr[48 * C];
        /* Stale audio left by stopped I/O: 7000 frames of -1. */
        for (int i = 0; i < 7000; i++) {
            for (int c = 0; c < C; c++) {
                wr[c] = -1.0f;
            }
            lw_ring_write(region2, LW_FROM_NET, wr, 1);
        }
        CHECK(DRV->StartIO(gDrv, 2, 1) == 0, "StartIO");
        /* Daemon writes 48 frames/ms (ramp on channel 1); host reads a block when due. */
        uint64_t written = 0, reads = 0, silent_blocks = 0, stale = 0, breaks = 0;
        float expect = -1.0f;
        for (int ms = 0; ms < 3000; ms++) {
            for (int i = 0; i < 48; i++) {
                for (int c = 0; c < C; c++) {
                    wr[i * C + c] = c == 0 ? (float)(written + (uint64_t)i) : 0.5f;
                }
            }
            /* Read block during writing: like host, without waiting for a tick boundary. */
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
        CHECK(reads >= 34 && silent_blocks == 0, "%llu blocks read, no silent block", (unsigned long long)reads);
        CHECK(stale == 0, "stale audio discarded at start (%llu stale frames read)", (unsigned long long)stale);
        CHECK(breaks == 0, "ramp continuous across blocks (%llu breaks)", (unsigned long long)breaks);
        /* Underrun: daemon stops for 200 ms; silence, then clean resumption. */
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        int zero = 1;
        for (int i = 0; i < B * C; i++) {
            zero &= blk[i] == 0.0f;
        }
        CHECK(zero, "underrun: silent block");
        for (int i = 0; i < B + 300; i++) {
            for (int c = 0; c < C; c++) {
                wr[c] = 0.25f;
            }
            lw_ring_write(region2, LW_FROM_NET, wr, 1);
        }
        DRV->DoIOOperation(gDrv, 2, 3, 1, kAudioServerPlugInIOOperationReadInput, B, &cycle_info, blk, NULL);
        CHECK(blk[0] == 0.25f && blk[B * C - 1] == 0.25f, "repriming after the underrun");
        CHECK(DRV->StopIO(gDrv, 2, 1) == 0, "StopIO");
    }
    printf("Numbered devices (multi layout)\n");
    lw_ring_spec spec3[3] = {{LW_TO_NET, 2}, {LW_FROM_NET, 1}, {LW_FROM_NET, 8}};
    size_t rsize3 = lw_shm_size(8192, 3, spec3);
    void *shmem3 = NULL;
    void *region3 = lw_shm_alloc(rsize3, &shmem3);
    lw_ring_spec spec4[3] = {{LW_TO_NET, 2}, {LW_FROM_NET, 2}, {LW_FROM_NET, 8}};
    size_t rsize4 = lw_shm_size(8192, 3, spec4);
    void *shmem4 = NULL;
    void *region4 = lw_shm_alloc(rsize4, &shmem4);
    {
        CHECK(region3 && lw_shm_init(region3, rsize3, 48000, 8192, 3, spec3, &clock) == 0,
              "region #3: Out 1 (2 ch), In 1 (1 ch), In 2 (8 ch)");
        lw_xpc_server_set_shmem(server, shmem3);
        UInt32 before = gPropertyChanges, req = gConfigRequests;
        gGen = 3;
        gNames = ",\"layout\":\"multi\",\"in_widths\":[1,8],\"out_widths\":[2],"
                 "\"in_device_names\":[\"OpenLW In - Studio A@Omnia One (ch. 2, L+R)\",\"\"],"
                 "\"out_device_names\":[\"OpenLW Out 1\"],"
                 "\"input_names\":[\"2 - Studio A (L+R)\",\"5 1\",\"5 2\"],\"output_names\":[\"4005 - MAC L\",\"4005 - MAC R\"]";
        poll();
        AudioObjectPropertyAddress la = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, 0};
        AudioObjectID devs[40] = {0};
        UInt32 sz = sizeof devs;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &la, 0, NULL, sz, &sz, devs);
        CHECK(sz == 3 * sizeof(AudioObjectID) && devs[0] == 100 && devs[1] == 102 && devs[2] == 200,
              "list: %u device(s), %u, %u, %u", (unsigned)(sz / sizeof(AudioObjectID)), devs[0], devs[1], devs[2]);
        CHECK(gPropertyChanges > before, "list change reported to the host");
        CHECK(gConfigRequests == req, "new devices: no configuration change request");
        char name[128];
        CHECK(get_str(100, kAudioObjectPropertyName, name, sizeof name) == 0 &&
                  strcmp(name, "OpenLW In - Studio A@Omnia One (ch. 2, L+R)") == 0,
              "In 1: %s", name);
        CHECK(get_str(102, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW In 2") == 0,
              "In 2 (no name from the daemon): %s", name);
        CHECK(get_str(200, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW Out 1") == 0,
              "Out 1: %s", name);
        CHECK(get_str(102, kAudioDevicePropertyDeviceUID, name, sizeof name) == 0 &&
                  strcmp(name, "fr.francois-brille.openlw.device.in.2") == 0,
              "UID: %s", name);
        CHECK(get_str(200, kAudioDevicePropertyDeviceUID, name, sizeof name) == 0 &&
                  strcmp(name, "fr.francois-brille.openlw.device.out.1") == 0,
              "UID: %s", name);
        AudioObjectPropertyAddress ta = {kAudioPlugInPropertyTranslateUIDToDevice, kAudioObjectPropertyScopeGlobal, 0};
        CFStringRef u = CFSTR("fr.francois-brille.openlw.device.in.2");
        AudioObjectID found = 0;
        sz = sizeof found;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &ta, sizeof u, &u, sz, &sz, &found);
        CHECK(found == 102, "TranslateUIDToDevice(in.2) → %u", found);
        u = CFSTR("fr.francois-brille.openlw.device");
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &ta, sizeof u, &u, sz, &sz, &found);
        CHECK(found == kAudioObjectUnknown, "duplex UID unknown in multi layout");
        AudioObjectPropertyAddress na = {kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, 0};
        CHECK(!DRV->HasProperty(gDrv, 2, getpid(), &na) && !DRV->HasProperty(gDrv, 3, getpid(), &na) &&
                  !DRV->HasProperty(gDrv, 104, getpid(), &na),
              "unpublished objects (duplex, In 3) answer no property");
        AudioObjectPropertyAddress sa = {kAudioDevicePropertyStreams, kAudioObjectPropertyScopeGlobal, 0};
        AudioObjectID st[2] = {0};
        sz = sizeof st;
        DRV->GetPropertyData(gDrv, 100, getpid(), &sa, 0, NULL, sz, &sz, st);
        CHECK(sz == sizeof(AudioObjectID) && st[0] == 101, "In 1: one input stream (101)");
        sz = sizeof st;
        DRV->GetPropertyData(gDrv, 200, getpid(), &sa, 0, NULL, sz, &sz, st);
        CHECK(sz == sizeof(AudioObjectID) && st[0] == 201, "Out 1: one output stream (201)");
        AudioObjectPropertyAddress fa = {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, 0};
        AudioStreamBasicDescription f1, f2, fo;
        sz = sizeof f1;
        DRV->GetPropertyData(gDrv, 101, getpid(), &fa, 0, NULL, sz, &sz, &f1);
        sz = sizeof f2;
        DRV->GetPropertyData(gDrv, 103, getpid(), &fa, 0, NULL, sz, &sz, &f2);
        sz = sizeof fo;
        DRV->GetPropertyData(gDrv, 201, getpid(), &fa, 0, NULL, sz, &sz, &fo);
        CHECK(f1.mChannelsPerFrame == 1 && f2.mChannelsPerFrame == 8 && fo.mChannelsPerFrame == 2,
              "widths follow the sources: %u, %u, %u", f1.mChannelsPerFrame, f2.mChannelsPerFrame, fo.mChannelsPerFrame);
        AudioObjectPropertyAddress ea = {kAudioObjectPropertyElementName, kAudioObjectPropertyScopeInput, 2};
        CFStringRef s = NULL;
        sz = sizeof s;
        CHECK(DRV->GetPropertyData(gDrv, 102, getpid(), &ea, 0, NULL, sz, &sz, &s) == 0 && s &&
                  CFStringGetCString(s, name, sizeof name, kCFStringEncodingUTF8) && strcmp(name, "5 2") == 0,
              "In 2, input 2 (names spread over devices): %s", name);
        if (s) CFRelease(s);
        ea.mElement = 2;
        CHECK(!DRV->HasProperty(gDrv, 100, getpid(), &ea), "In 1 has a single channel");

        /* Each device reads its own ring. */
        CHECK(DRV->StartIO(gDrv, 100, 1) == 0 && DRV->StartIO(gDrv, 102, 2) == 0 && DRV->StartIO(gDrv, 200, 3) == 0,
              "StartIO on three devices");
        static float one[1024], eight[1024 * 8], got1[512], got8[512 * 8], two[512 * 2], got2[512 * 2];
        for (int i = 0; i < 1024; i++) {
            one[i] = 0.5f;
        }
        for (int i = 0; i < 1024 * 8; i++) {
            eight[i] = 0.25f;
        }
        lw_ring_write(region3, 1, one, 1024);
        lw_ring_write(region3, 2, eight, 1024);
        DRV->DoIOOperation(gDrv, 100, 101, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got1, NULL);
        DRV->DoIOOperation(gDrv, 102, 103, 2, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got8, NULL);
        int ok1 = 1, ok8 = 1;
        for (int i = 0; i < 512; i++) {
            ok1 &= got1[i] == 0.5f;
        }
        for (int i = 0; i < 512 * 8; i++) {
            ok8 &= got8[i] == 0.25f;
        }
        CHECK(ok1 && ok8, "In 1 (1 ch) and In 2 (8 ch) read their own rings, no crosstalk");
        for (int i = 0; i < 512 * 2; i++) {
            two[i] = -0.75f;
        }
        DRV->DoIOOperation(gDrv, 200, 201, 3, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, two, NULL);
        lw_ring_read(region3, 0, got2, 512);
        CHECK(got2[0] == -0.75f && got2[512 * 2 - 1] == -0.75f, "Out 1 writes its ring");
        CHECK(DRV->DoIOOperation(gDrv, 100, 201, 1, kAudioServerPlugInIOOperationWriteMix, 512, &cycle_info, two, NULL) != 0,
              "stream of another device rejected");

        /* In 1 widened to 2 (stereo patch): new region, one request, for In 1 only. In 2
         * resumes on the new region at once; In 1 stays silent until reconfigured. */
        CHECK(region4 && lw_shm_init(region4, rsize4, 48000, 8192, 3, spec4, &clock) == 0, "region #4: In 1 at 2 ch");
        lw_xpc_server_set_shmem(server, shmem4);
        req = gConfigRequests;
        gGen = 4;
        gNames = ",\"layout\":\"multi\",\"in_widths\":[2,8],\"out_widths\":[2]";
        poll();
        CHECK(gConfigRequests == req + 1, "one configuration change request (%u)", gConfigRequests - req);
        static float stereo[1024 * 2];
        for (int i = 0; i < 1024 * 2; i++) {
            stereo[i] = 0.125f;
        }
        lw_ring_write(region4, 1, stereo, 1024);
        lw_ring_write(region4, 2, eight, 1024);
        for (int i = 0; i < 512; i++) {
            got1[i] = 1.0f;
        }
        DRV->DoIOOperation(gDrv, 102, 103, 2, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got8, NULL);
        DRV->DoIOOperation(gDrv, 100, 101, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got1, NULL);
        int silent1 = 1;
        for (int i = 0; i < 512; i++) {
            silent1 &= got1[i] == 0.0f;
        }
        CHECK(got8[0] == 0.25f && got8[512 * 8 - 1] == 0.25f, "In 2 (width unchanged) reads the new region");
        CHECK(silent1, "In 1 awaiting reconfiguration: silence, 2-channel ring never read into a 1-channel buffer");
        DRV->StopIO(gDrv, 100, 1);
        CHECK(DRV->PerformDeviceConfigurationChange(gDrv, 100, 0, NULL) == 0, "change applied to In 1");
        sz = sizeof f1;
        DRV->GetPropertyData(gDrv, 101, getpid(), &fa, 0, NULL, sz, &sz, &f1);
        CHECK(f1.mChannelsPerFrame == 2, "In 1 now %u channels", f1.mChannelsPerFrame);
        CHECK(DRV->StartIO(gDrv, 100, 1) == 0, "StartIO In 1");
        lw_ring_write(region4, 1, stereo, 1024);
        DRV->DoIOOperation(gDrv, 100, 101, 1, kAudioServerPlugInIOOperationReadInput, 512, &cycle_info, got2, NULL);
        CHECK(got2[0] == 0.125f && got2[512 * 2 - 1] == 0.125f, "In 1 reads its 2-channel ring");

        /* Renaming a device: notified without configuration change. */
        before = gPropertyChanges;
        req = gConfigRequests;
        gNames = ",\"layout\":\"multi\",\"in_widths\":[2,8],\"out_widths\":[2],\"in_device_names\":[\"\",\"OpenLW In - X (ch. 9)\"]";
        poll();
        CHECK(get_str(102, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW In - X (ch. 9)") == 0,
              "In 2 renamed: %s", name);
        CHECK(gPropertyChanges > before && gConfigRequests == req, "rename notified, no configuration change");

        /* Fewer devices, then back to duplex. */
        DRV->StopIO(gDrv, 100, 1);
        DRV->StopIO(gDrv, 102, 2);
        DRV->StopIO(gDrv, 200, 3);
        gNames = ",\"layout\":\"multi\",\"in_widths\":[2],\"out_widths\":[2]";
        poll();
        sz = sizeof devs;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &la, 0, NULL, sz, &sz, devs);
        CHECK(sz == 2 * sizeof(AudioObjectID) && devs[0] == 100 && devs[1] == 200, "In 2 removed");
        gNames = "";
        poll();
        sz = sizeof devs;
        DRV->GetPropertyData(gDrv, kAudioObjectPlugInObject, getpid(), &la, 0, NULL, sz, &sz, devs);
        CHECK(sz == sizeof(AudioObjectID) && devs[0] == 2, "back to one duplex device");
        CHECK(get_str(2, kAudioObjectPropertyName, name, sizeof name) == 0 && strcmp(name, "OpenLW") == 0, "name: %s", name);
        CHECK(!DRV->HasProperty(gDrv, 100, getpid(), &na), "In 1 no longer published");
    }

    use_endpoint(NULL);
    lw_xpc_release(endpoint);
    lw_xpc_server_stop(server);
    lw_xpc_release(shmem);
    lw_shm_unmap(region, rsize);
    lw_xpc_release(shmem2);
    lw_shm_unmap(region2, rsize2);
    lw_xpc_release(shmem3);
    lw_shm_unmap(region3, rsize3);
    lw_xpc_release(shmem4);
    lw_shm_unmap(region4, rsize4);
    free(buf);
    free(src);
    free(ahead);

    printf("%s: %d failure(s)\n", gFailures ? "FAIL" : "SUCCESS", gFailures);
    return gFailures ? 1 : 0;
}
