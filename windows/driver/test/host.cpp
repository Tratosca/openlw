// Hôte de test du pilote audio Windows d'OpenLW : charge la DLL comme un logiciel ASIO, joue une
// sinusoïde sur deux sorties et vérifie qu'elle revient sur deux entrées par la boucle interne du
// service (configuration "device": {"loopback": true}).
//
//   openlw-driver-test [chemin\OpenLWDriver.dll]          essai complet
//   openlw-driver-test --expect-busy [chemin]             init doit échouer : pilote déjà utilisé
//   openlw-driver-test --hold SECONDES [chemin]           garde le pilote attaché (pour --expect-busy)
//
// Copyright 2026 François Brille (Tratosca). Licence GPL version 3 (fichier LICENSE).
// ASIO is a registered trademark of Steinberg Media Technologies GmbH.

#include <windows.h>
#include <objbase.h>

#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "asiosys.h"
#include "asio.h"
#include "iasiodrv.h"

static const CLSID CLSID_OpenLW = {0x4f9dd084, 0xe18a, 0x4e0e, {0x8d, 0x38, 0xc2, 0x85, 0x40, 0x29, 0xe8, 0x46}};

static int g_failures = 0;
#define CHECK(cond, ...)                                                                                          \
    do {                                                                                                         \
        bool ok_ = (cond);                                                                                       \
        std::printf("  %s ", ok_ ? "ok  " : "ÉCHEC");                                                            \
        std::printf(__VA_ARGS__);                                                                                \
        std::printf("\n");                                                                                       \
        if (!ok_) {                                                                                              \
            g_failures++;                                                                                        \
        }                                                                                                        \
    } while (0)

namespace {

constexpr long kSize = 256;
constexpr float kAmp = 0.5f;

ASIOBufferInfo g_infos[4];
std::atomic<long> g_switches{0};
std::atomic<long> g_time_info{0};
uint64_t g_phase = 0;
std::vector<float> g_received; // entrée 1 (après amorçage)
float g_peak = 0.0f;
long g_mismatch = 0;

void process(long index) {
    float *out0 = static_cast<float *>(g_infos[2].buffers[index]);
    float *out1 = static_cast<float *>(g_infos[3].buffers[index]);
    const float *in0 = static_cast<const float *>(g_infos[0].buffers[index]);
    const float *in1 = static_cast<const float *>(g_infos[1].buffers[index]);
    for (long i = 0; i < kSize; i++) {
        float s = kAmp * static_cast<float>(std::sin(2.0 * 3.141592653589793 * 1000.0 * double(g_phase++) / 48000.0));
        out0[i] = s;
        out1[i] = -s;
        if (std::fabs(in0[i]) > 0.0f) {
            g_peak = std::fmax(g_peak, std::fabs(in0[i]));
            if (in1[i] != -in0[i]) {
                g_mismatch++;
            }
            if (g_received.size() < 48000) {
                g_received.push_back(in0[i]);
            }
        }
    }
    g_switches++;
}

void buffer_switch(long index, ASIOBool) { process(index); }

ASIOTime *buffer_switch_time_info(ASIOTime *params, long index, ASIOBool) {
    if (params != nullptr && (params->timeInfo.flags & kSamplePositionValid)) {
        g_time_info++;
    }
    process(index);
    return params;
}

void sample_rate_changed(ASIOSampleRate) {}

long asio_message(long selector, long value, void *, double *) {
    switch (selector) {
    case kAsioSelectorSupported:
        return value == kAsioSupportsTimeInfo || value == kAsioResetRequest || value == kAsioEngineVersion ? 1 : 0;
    case kAsioSupportsTimeInfo:
        return 1;
    case kAsioEngineVersion:
        return 2;
    case kAsioResetRequest:
        std::printf("  info demande de réinitialisation reçue\n");
        return 1;
    default:
        return 0;
    }
}

IASIO *load(const wchar_t *dll, HMODULE *module) {
    *module = LoadLibraryW(dll);
    if (*module == nullptr) {
        std::printf("  ÉCHEC chargement de la DLL (erreur %lu)\n", GetLastError());
        return nullptr;
    }
    using GetClassObject = HRESULT(STDAPICALLTYPE *)(REFCLSID, REFIID, LPVOID *);
    auto get = reinterpret_cast<GetClassObject>(reinterpret_cast<void *>(GetProcAddress(*module, "DllGetClassObject")));
    IClassFactory *factory = nullptr;
    if (get == nullptr || FAILED(get(CLSID_OpenLW, IID_IClassFactory, reinterpret_cast<void **>(&factory)))) {
        std::printf("  ÉCHEC fabrique COM introuvable\n");
        return nullptr;
    }
    IASIO *driver = nullptr;
    // Convention ASIO : l'interface est demandée par le CLSID du pilote.
    HRESULT hr = factory->CreateInstance(nullptr, CLSID_OpenLW, reinterpret_cast<void **>(&driver));
    factory->Release();
    return SUCCEEDED(hr) ? driver : nullptr;
}

} // namespace

int wmain(int argc, wchar_t **argv) {
    bool expect_busy = false;
    int hold = 0;
    std::wstring dll = L"OpenLWDriver.dll";
    for (int i = 1; i < argc; i++) {
        if (std::wcscmp(argv[i], L"--expect-busy") == 0) {
            expect_busy = true;
        } else if (std::wcscmp(argv[i], L"--hold") == 0 && i + 1 < argc) {
            hold = _wtoi(argv[++i]);
        } else {
            dll = argv[i];
        }
    }
    CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    HMODULE module = nullptr;
    std::printf("chargement\n");
    IASIO *d = load(dll.c_str(), &module);
    CHECK(d != nullptr, "instance du pilote créée");
    if (d == nullptr) {
        return 1;
    }
    char name[33] = {0}, msg[125] = {0};
    d->getDriverName(name);
    CHECK(std::strcmp(name, "OpenLW") == 0, "nom affiché « %s »", name);
    ASIOBool inited = d->init(nullptr);
    d->getErrorMessage(msg);
    if (expect_busy) {
        CHECK(!inited, "init refusé pendant qu'un autre processus utilise le pilote : %s", msg);
        d->Release();
        return g_failures == 0 ? 0 : 1;
    }
    CHECK(inited, "init (connexion au service, région partagée) %s", inited ? "" : msg);
    if (!inited) {
        d->Release();
        return 1;
    }
    if (hold > 0) {
        std::printf("pilote attaché pendant %d s\n", hold);
        std::fflush(stdout);
        Sleep(static_cast<DWORD>(hold) * 1000);
        d->Release();
        return 0;
    }

    std::printf("propriétés\n");
    long ins = 0, outs = 0, mn = 0, mx = 0, pref = 0, gran = 0;
    CHECK(d->getChannels(&ins, &outs) == ASE_OK && ins >= 2 && outs >= 2, "%ld entrées, %ld sorties", ins, outs);
    CHECK(d->getBufferSize(&mn, &mx, &pref, &gran) == ASE_OK && mn <= kSize && kSize <= mx,
          "tampons %ld à %ld, préféré %ld", mn, mx, pref);
    CHECK(d->canSampleRate(48000.0) == ASE_OK && d->canSampleRate(44100.0) != ASE_OK, "48 kHz seulement");
    ASIOChannelInfo ci;
    std::memset(&ci, 0, sizeof ci);
    ci.channel = 0;
    ci.isInput = ASIOTrue;
    CHECK(d->getChannelInfo(&ci) == ASE_OK && ci.type == ASIOSTFloat32LSB, "entrée 1 « %s », float 32 bits", ci.name);
    ASIOClockSource clocks[2];
    long nclocks = 2;
    CHECK(d->getClockSources(clocks, &nclocks) == ASE_OK && nclocks == 1, "une source d'horloge (%s)", clocks[0].name);

    std::printf("audio (boucle interne du service)\n");
    for (int i = 0; i < 4; i++) {
        g_infos[i].isInput = i < 2 ? ASIOTrue : ASIOFalse;
        g_infos[i].channelNum = i % 2;
        g_infos[i].buffers[0] = g_infos[i].buffers[1] = nullptr;
    }
    ASIOCallbacks cb = {buffer_switch, sample_rate_changed, asio_message, buffer_switch_time_info};
    CHECK(d->createBuffers(g_infos, 4, kSize, &cb) == ASE_OK, "createBuffers (2 entrées, 2 sorties, %ld trames)", kSize);
    CHECK(d->start() == ASE_OK, "start");
    Sleep(3000);
    ASIOSamples pos;
    ASIOTimeStamp ts;
    CHECK(d->getSamplePosition(&pos, &ts) == ASE_OK && pos.lo > 0, "position d'échantillon %lu", pos.lo);
    CHECK(d->stop() == ASE_OK, "stop");
    long expected = 3 * 48000 / kSize;
    CHECK(g_switches > expected * 8 / 10 && g_switches < expected * 12 / 10, "%ld échanges de tampons (%ld attendus)",
          static_cast<long>(g_switches), expected);
    CHECK(g_time_info == g_switches, "mode ASIOTime (%ld)", static_cast<long>(g_time_info));
    CHECK(std::fabs(g_peak - kAmp) < 0.01f, "sinusoïde revenue par le réseau, crête %.3f", g_peak);
    CHECK(g_mismatch == 0, "canaux intacts (%ld écarts)", g_mismatch);
    long jumps = 0;
    for (size_t i = 2; i < g_received.size(); i++) {
        // Continuité : la dérivée seconde d'une sinusoïde à 1 kHz reste faible.
        float d2 = g_received[i] - 2 * g_received[i - 1] + g_received[i - 2];
        if (std::fabs(d2) > 0.05f) {
            jumps++;
        }
    }
    CHECK(g_received.size() > 24000 && jumps <= 2, "%zu échantillons reçus, %ld discontinuité(s)", g_received.size(),
          jumps);
    CHECK(d->disposeBuffers() == ASE_OK, "disposeBuffers");
    d->Release();
    std::printf("%s : %d échec(s)\n", g_failures == 0 ? "SUCCÈS" : "ÉCHEC", g_failures);
    return g_failures == 0 ? 0 : 1;
}
