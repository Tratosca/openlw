// OpenLW — Windows audio driver, ASIO interface.
// Copyright 2026 François Brille (Tratosca). GPL version 3 (LICENSE file): driver
// is built with Steinberg ASIO SDK, used under GPLv3.
// ASIO is a registered trademark of Steinberg Media Technologies GmbH.
//
// In-process COM DLL loaded by host application. Driver:
// - connects to OpenLW service through named pipe, reads device geometry;
// - obtains shared region (`attach`), duplicated into this process by service;
// - paces buffer exchanges using published region clock (QueryPerformanceCounter),
// in an MMCSS “Pro Audio” thread: FROM_NET ring → host inputs, host outputs →
//   TO_NET ring;
// - monitors geometry and requests host reset when device changes.
//
// Clean-room: relies only on ASIO SDK, Microsoft documentation, and docs/protocol/.

#include <windows.h>
#include <objbase.h>
#include <mmsystem.h>
#include <shellapi.h>

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <new>
#include <string>
#include <vector>

extern "C" {
#include "lw_shm.h"
#include "lw_sys.h"
}

#include "asiosys.h"
#include "asio.h"
#include "iasiodrv.h"

// {4F9DD084-E18A-4E0E-8D38-C2854029E846}
static const CLSID CLSID_OpenLW = {0x4f9dd084, 0xe18a, 0x4e0e, {0x8d, 0x38, 0xc2, 0x85, 0x40, 0x29, 0xe8, 0x46}};

namespace {

constexpr const char *kServicePipe = "fr.francois-brille.openlw.daemon";
constexpr const wchar_t *kDriverName = L"OpenLW"; // Host-displayed name (HKLM\SOFTWARE\ASIO key)
constexpr long kVersion = 5;                     // 0.5
constexpr double kRate = 48000.0;
constexpr long kMinBuffer = 64, kMaxBuffer = 2048, kPreferredBuffer = 256;

HINSTANCE g_module = nullptr;
std::atomic<long> g_objects{0};
std::atomic<long> g_locks{0};

void log_msg(int level, const std::string &msg) { lw_log(level, "driver", msg.c_str()); }

// UTF-8 → ANSI code page (ASIO hosts display ANSI strings), truncated to cap - 1.
void to_ansi(const std::string &utf8, char *out, size_t cap) {
    if (cap == 0) {
        return;
    }
    out[0] = '\0';
    int n = MultiByteToWideChar(CP_UTF8, 0, utf8.c_str(), -1, nullptr, 0);
    if (n <= 0) {
        return;
    }
    std::wstring w(static_cast<size_t>(n), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.c_str(), -1, w.data(), n);
    if (WideCharToMultiByte(CP_ACP, 0, w.c_str(), -1, out, static_cast<int>(cap), "?", nullptr) == 0) {
        out[cap - 1] = '\0';
    }
}

// ---------- Service JSON response parsing (known format, no ambiguous nesting) ----------

bool json_u64(const std::string &j, const char *key, uint64_t *out) {
    std::string pat = std::string("\"") + key + "\":";
    size_t p = j.find(pat);
    if (p == std::string::npos) {
        return false;
    }
    p += pat.size();
    while (p < j.size() && j[p] == ' ') {
        p++;
    }
    char *end = nullptr;
    unsigned long long v = std::strtoull(j.c_str() + p, &end, 10);
    if (end == j.c_str() + p) {
        return false;
    }
    *out = v;
    return true;
}

// JSON string starting after opening quote; return position after closing quote.
size_t json_str(const std::string &j, size_t p, std::string *out) {
    out->clear();
    while (p < j.size() && j[p] != '"') {
        char c = j[p++];
        if (c != '\\' || p >= j.size()) {
            out->push_back(c);
            continue;
        }
        char e = j[p++];
        switch (e) {
        case 'n': out->push_back('\n'); break;
        case 't': out->push_back('\t'); break;
        case 'u': {
            unsigned v = 0;
            for (int i = 0; i < 4 && p < j.size(); i++) {
                char h = j[p++];
                v = v * 16 + static_cast<unsigned>(h >= 'a' ? h - 'a' + 10 : h >= 'A' ? h - 'A' + 10 : h - '0');
            }
            // Basic multilingual plane characters encoded as UTF-8 (surrogate pairs ignored).
            if (v < 0x80) {
                out->push_back(static_cast<char>(v));
            } else if (v < 0x800) {
                out->push_back(static_cast<char>(0xC0 | (v >> 6)));
                out->push_back(static_cast<char>(0x80 | (v & 0x3F)));
            } else {
                out->push_back(static_cast<char>(0xE0 | (v >> 12)));
                out->push_back(static_cast<char>(0x80 | ((v >> 6) & 0x3F)));
                out->push_back(static_cast<char>(0x80 | (v & 0x3F)));
            }
            break;
        }
        default: out->push_back(e); break;
        }
    }
    return p < j.size() ? p + 1 : p;
}

bool json_string(const std::string &j, const char *key, std::string *out) {
    std::string pat = std::string("\"") + key + "\":\"";
    size_t p = j.find(pat);
    if (p == std::string::npos) {
        return false;
    }
    json_str(j, p + pat.size(), out);
    return true;
}

std::vector<std::string> json_strings(const std::string &j, const char *key) {
    std::vector<std::string> v;
    std::string pat = std::string("\"") + key + "\":[";
    size_t p = j.find(pat);
    if (p == std::string::npos) {
        return v;
    }
    p += pat.size();
    while (p < j.size() && j[p] != ']') {
        if (j[p] == '"') {
            std::string s;
            p = json_str(j, p + 1, &s);
            v.push_back(s);
        } else {
            p++;
        }
    }
    return v;
}

// ---------- Service connection ----------

class Control {
public:
    ~Control() { close(); }
    bool open(std::string *error) {
        close();
        const char *err = nullptr;
        client_ = lw_pipe_client_connect(kServicePipe, 2000, &err);
        if (client_ == nullptr) {
            *error = std::string("OpenLW service unreachable: ") + (err ? err : "unknown error");
            return false;
        }
        return true;
    }
    void close() {
        if (client_ != nullptr) {
            lw_pipe_client_close(client_);
            client_ = nullptr;
        }
    }
    bool call(const char *request, std::string *reply, std::string *error) {
        if (client_ == nullptr && !open(error)) {
            return false;
        }
        const char *err = nullptr;
        char *out = lw_pipe_call(client_, request, &err);
        if (out == nullptr) {
            *error = std::string("OpenLW service: ") + (err ? err : "unknown error");
            close();
            return false;
        }
        reply->assign(out);
        lw_free(out);
        return true;
    }

private:
    lw_pipe_client *client_ = nullptr;
};

struct Geometry {
    uint64_t generation = 0, to_net = 0, from_net = 0, margin = 256;
    std::vector<std::string> input_names, output_names;
};

bool read_geometry(Control &c, Geometry *g, std::string *error) {
    std::string r;
    if (!c.call("{\"cmd\":\"geometry\"}", &r, error)) {
        return false;
    }
    if (r.find("\"ok\":true") == std::string::npos) {
        std::string e;
        *error = json_string(r, "error", &e) ? e : "OpenLW device unavailable";
        return false;
    }
    if (!json_u64(r, "generation", &g->generation) || !json_u64(r, "channels_to_net", &g->to_net) ||
        !json_u64(r, "channels_from_net", &g->from_net)) {
        *error = "incomplete service response";
        return false;
    }
    uint64_t m = 0;
    if (json_u64(r, "input_margin", &m) && m >= 64 && m <= 2048) {
        g->margin = m;
    }
    g->input_names = json_strings(r, "input_names");
    g->output_names = json_strings(r, "output_names");
    return true;
}

void split64(uint64_t v, unsigned long *hi, unsigned long *lo) {
    *hi = static_cast<unsigned long>(v >> 32);
    *lo = static_cast<unsigned long>(v & 0xFFFFFFFFu);
}

// ---------- Driver ----------

class Driver final : public IASIO {
public:
    Driver() { g_objects++; }
    ~Driver() {
        stop();
        disposeBuffers();
        stop_monitor();
        if (base_ != nullptr) {
            std::string r, e;
            control_.call("{\"cmd\":\"detach\"}", &r, &e);
            lw_shm_unmap(base_, size_);
            lw_shm_release(handle_);
        }
        g_objects--;
    }

    // IUnknown. ASIO hosts request interface through driver CLSID.
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void **ppv) override {
        if (ppv == nullptr) {
            return E_POINTER;
        }
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, CLSID_OpenLW)) {
            *ppv = static_cast<IASIO *>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return static_cast<ULONG>(++refs_); }
    ULONG STDMETHODCALLTYPE Release() override {
        long n = --refs_;
        if (n == 0) {
            delete this;
        }
        return static_cast<ULONG>(n);
    }

    ASIOBool init(void *sysHandle) override {
        (void)sysHandle;
        if (base_ != nullptr) {
            return ASIOTrue;
        }
        std::string err;
        if (!control_.open(&err) || !read_geometry(control_, &geom_, &err)) {
            return fail(err);
        }
        std::string r;
        if (!control_.call("{\"cmd\":\"attach\",\"want_shmem\":true}", &r, &err)) {
            return fail(err);
        }
        uint64_t h = 0;
        if (r.find("\"ok\":true") == std::string::npos || !json_u64(r, "handle", &h) || h == 0) {
            std::string e;
            return fail(json_string(r, "error", &e) ? e : "shared region refused by the service");
        }
        handle_ = reinterpret_cast<void *>(static_cast<uintptr_t>(h));
        base_ = lw_shm_map(handle_, &size_);
        lw_host_clock clock{};
        if (base_ != nullptr && lw_shm_validate(base_, size_) == 0) {
            lw_shm_host_clock(base_, &clock);
        }
        // Duplex region only (two rings: TO_NET then FROM_NET); ASIO exposes one device.
        if (base_ == nullptr || clock.id != LW_CLOCK_QPC || lw_shm_sample_rate(base_) != static_cast<uint32_t>(kRate) ||
            lw_shm_ring_count(base_) != 2 || lw_ring_dir(base_, LW_FROM_NET) != LW_FROM_NET ||
            lw_ring_channels(base_, LW_TO_NET) != geom_.to_net ||
            lw_ring_channels(base_, LW_FROM_NET) != geom_.from_net) {
            if (base_ != nullptr) {
                lw_shm_unmap(base_, size_);
                base_ = nullptr;
            }
            lw_shm_release(handle_);
            handle_ = nullptr;
            return fail("invalid shared region (version, clock or channel count)");
        }
        qpc_freq_ = clock.ns_denom;
        start_monitor();
        log_msg(1, "attached: " + std::to_string(geom_.from_net) + " inputs, " + std::to_string(geom_.to_net) +
                       " outputs, generation " + std::to_string(geom_.generation));
        return ASIOTrue;
    }

    void getDriverName(char *name) override { std::snprintf(name, 32, "OpenLW"); }
    long getDriverVersion() override { return kVersion; }
    void getErrorMessage(char *string) override {
        std::lock_guard<std::mutex> l(error_mutex_);
        to_ansi(error_.empty() ? "no error" : error_, string, 124);
    }

    ASIOError start() override {
        if (buffer_size_ == 0 || callbacks_ == nullptr) {
            return ASE_InvalidMode;
        }
        if (running_) {
            return ASE_OK;
        }
        running_ = true;
        frames_ = 0;
        audio_thread_ = CreateThread(nullptr, 0, &Driver::audio_main, this, 0, nullptr);
        if (audio_thread_ == nullptr) {
            running_ = false;
            return ASE_HWMalfunction;
        }
        return ASE_OK;
    }

    ASIOError stop() override {
        if (!running_) {
            return ASE_OK;
        }
        running_ = false;
        WaitForSingleObject(audio_thread_, INFINITE);
        CloseHandle(audio_thread_);
        audio_thread_ = nullptr;
        return ASE_OK;
    }

    ASIOError getChannels(long *numInputChannels, long *numOutputChannels) override {
        if (base_ == nullptr) {
            return ASE_NotPresent;
        }
        *numInputChannels = static_cast<long>(geom_.from_net);
        *numOutputChannels = static_cast<long>(geom_.to_net);
        return ASE_OK;
    }

    ASIOError getLatencies(long *inputLatency, long *outputLatency) override {
        long n = buffer_size_ != 0 ? buffer_size_ : kPreferredBuffer;
        *inputLatency = n + static_cast<long>(geom_.margin);
        *outputLatency = n;
        return ASE_OK;
    }

    ASIOError getBufferSize(long *minSize, long *maxSize, long *preferredSize, long *granularity) override {
        *minSize = kMinBuffer;
        *maxSize = kMaxBuffer;
        *preferredSize = kPreferredBuffer;
        *granularity = -1; // Powers of two
        return ASE_OK;
    }

    ASIOError canSampleRate(ASIOSampleRate sampleRate) override {
        return sampleRate == kRate ? ASE_OK : ASE_NoClock;
    }
    ASIOError getSampleRate(ASIOSampleRate *sampleRate) override {
        *sampleRate = kRate;
        return ASE_OK;
    }
    ASIOError setSampleRate(ASIOSampleRate sampleRate) override {
        return sampleRate == kRate ? ASE_OK : ASE_NoClock;
    }

    ASIOError getClockSources(ASIOClockSource *clocks, long *numSources) override {
        if (clocks == nullptr || numSources == nullptr || *numSources < 1) {
            return ASE_InvalidParameter;
        }
        std::memset(clocks, 0, sizeof *clocks);
        clocks->index = 0;
        clocks->associatedChannel = -1;
        clocks->associatedGroup = -1;
        clocks->isCurrentSource = ASIOTrue;
        std::snprintf(clocks->name, sizeof clocks->name, "OpenLW");
        *numSources = 1;
        return ASE_OK;
    }
    ASIOError setClockSource(long reference) override { return reference == 0 ? ASE_OK : ASE_InvalidParameter; }

    ASIOError getSamplePosition(ASIOSamples *sPos, ASIOTimeStamp *tStamp) override {
        if (!running_) {
            return ASE_SPNotAdvancing;
        }
        uint64_t pos, time;
        {
            std::lock_guard<std::mutex> l(position_mutex_);
            pos = position_;
            time = position_time_ns_;
        }
        split64(pos, &sPos->hi, &sPos->lo);
        split64(time, &tStamp->hi, &tStamp->lo);
        return ASE_OK;
    }

    ASIOError getChannelInfo(ASIOChannelInfo *info) override {
        if (info == nullptr || base_ == nullptr) {
            return ASE_InvalidParameter;
        }
        uint64_t count = info->isInput ? geom_.from_net : geom_.to_net;
        if (info->channel < 0 || static_cast<uint64_t>(info->channel) >= count) {
            return ASE_InvalidParameter;
        }
        info->isActive = ASIOFalse;
        for (const Buf &b : buffers_) {
            if (b.input == (info->isInput != ASIOFalse) && b.channel == info->channel) {
                info->isActive = ASIOTrue;
            }
        }
        info->channelGroup = 0;
        info->type = ASIOSTFloat32LSB;
        const std::vector<std::string> &names = info->isInput ? geom_.input_names : geom_.output_names;
        size_t i = static_cast<size_t>(info->channel);
        std::string name = i < names.size() && !names[i].empty()
                               ? names[i]
                               : std::string(info->isInput ? "Input " : "Output ") + std::to_string(i + 1);
        to_ansi(name, info->name, sizeof info->name);
        return ASE_OK;
    }

    ASIOError createBuffers(ASIOBufferInfo *bufferInfos, long numChannels, long bufferSize,
                            ASIOCallbacks *callbacks) override {
        if (base_ == nullptr) {
            return ASE_NotPresent;
        }
        if (running_ || !buffers_.empty()) {
            return ASE_InvalidMode;
        }
        if (bufferSize < kMinBuffer || bufferSize > kMaxBuffer || (bufferSize & (bufferSize - 1)) != 0 ||
            callbacks == nullptr || bufferInfos == nullptr || numChannels <= 0) {
            return ASE_InvalidMode;
        }
        for (long i = 0; i < numChannels; i++) {
            ASIOBufferInfo &bi = bufferInfos[i];
            uint64_t count = bi.isInput ? geom_.from_net : geom_.to_net;
            if (bi.channelNum < 0 || static_cast<uint64_t>(bi.channelNum) >= count) {
                buffers_.clear();
                return ASE_InvalidParameter;
            }
            Buf b;
            b.input = bi.isInput != ASIOFalse;
            b.channel = bi.channelNum;
            b.data.assign(static_cast<size_t>(bufferSize) * 2, 0.0f);
            buffers_.push_back(std::move(b));
        }
        // Addresses fixed after filling vector (no subsequent reallocation).
        for (long i = 0; i < numChannels; i++) {
            bufferInfos[i].buffers[0] = buffers_[static_cast<size_t>(i)].data.data();
            bufferInfos[i].buffers[1] = buffers_[static_cast<size_t>(i)].data.data() + bufferSize;
        }
        buffer_size_ = bufferSize;
        callbacks_ = callbacks;
        in_frames_.assign(static_cast<size_t>(bufferSize) * geom_.from_net, 0.0f);
        out_frames_.assign(static_cast<size_t>(bufferSize) * geom_.to_net, 0.0f);
        time_info_ = callbacks->asioMessage != nullptr &&
                     callbacks->asioMessage(kAsioSelectorSupported, kAsioSupportsTimeInfo, nullptr, nullptr) == 1 &&
                     callbacks->asioMessage(kAsioSupportsTimeInfo, 0, nullptr, nullptr) == 1 &&
                     callbacks->bufferSwitchTimeInfo != nullptr;
        return ASE_OK;
    }

    ASIOError disposeBuffers() override {
        stop();
        buffers_.clear();
        buffer_size_ = 0;
        callbacks_ = nullptr;
        return ASE_OK;
    }

    // Control panel: OpenLW app installed beside driver.
    ASIOError controlPanel() override {
        wchar_t path[MAX_PATH];
        DWORD n = GetModuleFileNameW(g_module, path, MAX_PATH);
        if (n == 0 || n >= MAX_PATH) {
            return ASE_NotPresent;
        }
        wchar_t *slash = wcsrchr(path, L'\\');
        if (slash == nullptr) {
            return ASE_NotPresent;
        }
        wcscpy_s(slash + 1, MAX_PATH - static_cast<size_t>(slash + 1 - path), L"OpenLW.exe");
        HINSTANCE r = ShellExecuteW(nullptr, L"open", path, nullptr, nullptr, SW_SHOWNORMAL);
        return reinterpret_cast<INT_PTR>(r) > 32 ? ASE_OK : ASE_NotPresent;
    }

    ASIOError future(long selector, void *opt) override {
        (void)opt;
        return selector == kAsioCanTimeInfo ? ASE_SUCCESS : ASE_InvalidParameter;
    }
    ASIOError outputReady() override { return ASE_NotPresent; }

private:
    struct Buf {
        bool input = false;
        long channel = 0;
        std::vector<float> data; // Two halves of buffer_size_ samples
    };

    ASIOBool fail(const std::string &e) {
        {
            std::lock_guard<std::mutex> l(error_mutex_);
            error_ = e;
        }
        log_msg(3, e);
        return ASIOFalse;
    }

    // Instant (QPC ticks) at which service sample position equals `pos`, extrapolated from published clock.
    bool deadline_for(uint64_t pos, uint64_t *ticks, uint64_t *sample_now) {
        uint64_t ht = 0, st = 0;
        double rate = 1.0;
        if (lw_clock_read(base_, &ht, &st, &rate) != 0 || qpc_freq_ == 0) {
            return false;
        }
        uint64_t now = lw_host_time();
        int64_t elapsed = static_cast<int64_t>(now - ht);
        *sample_now = st + static_cast<uint64_t>(elapsed > 0 ? elapsed * static_cast<int64_t>(kRate) /
                                                                   static_cast<int64_t>(qpc_freq_)
                                                             : 0);
        int64_t delta = static_cast<int64_t>(pos) - static_cast<int64_t>(st);
        *ticks = ht + static_cast<uint64_t>(delta * static_cast<int64_t>(qpc_freq_) / static_cast<int64_t>(kRate));
        return true;
    }

    static DWORD WINAPI audio_main(LPVOID self) {
        static_cast<Driver *>(self)->run();
        return 0;
    }

    void run() {
        const uint64_t period = static_cast<uint64_t>(buffer_size_);
        int rc = lw_rt_promote(period * 1000000000ull / 48000, 0, 0);
        if (rc != 0) {
            log_msg(2, "MMCSS real-time scheduling refused (code " + std::to_string(rc) + ")");
        }
        const size_t n = static_cast<size_t>(buffer_size_);
        const size_t ch_in = static_cast<size_t>(geom_.from_net), ch_out = static_cast<size_t>(geom_.to_net);
        const uint64_t margin = geom_.margin;
        uint64_t ticks = 0, now_pos = 0;
        while (running_ && !deadline_for(0, &ticks, &now_pos)) {
            Sleep(5); // Clock not yet published
        }
        uint64_t next = now_pos + period;
        long index = 0;
        bool primed = false;
        while (running_) {
            if (!deadline_for(next, &ticks, &now_pos)) {
                Sleep(1);
                continue;
            }
            uint64_t now = lw_host_time();
            if (ticks > now) {
                lw_sleep_ns((ticks - now) * 1000000000ull / qpc_freq_);
            } else if (now_pos > next + 4 * period) {
                next = now_pos + period; // Large delay (suspended thread): realign
                primed = false;
                continue;
            }
            // Inputs: network audio with service-configured latency margin.
            uint32_t readable = lw_ring_readable(base_, LW_FROM_NET);
            if (!primed && readable >= period + margin) {
                primed = true;
            }
            if (primed) {
                if (readable > period + 2 * margin) {
                    lw_ring_skip(base_, LW_FROM_NET, static_cast<uint32_t>(readable - period - margin));
                }
                lw_ring_read(base_, LW_FROM_NET, in_frames_.data(), static_cast<uint32_t>(n));
            } else {
                std::fill(in_frames_.begin(), in_frames_.end(), 0.0f);
            }
            for (Buf &b : buffers_) {
                if (!b.input) {
                    continue;
                }
                float *dst = b.data.data() + static_cast<size_t>(index) * n;
                for (size_t f = 0; f < n; f++) {
                    dst[f] = in_frames_[f * ch_in + static_cast<size_t>(b.channel)];
                }
            }
            // Windows ASIO timestamp: derived from timeGetTime(), in nanoseconds.
            const uint64_t time_ns = static_cast<uint64_t>(timeGetTime()) * 1000000ull;
            {
                std::lock_guard<std::mutex> l(position_mutex_);
                position_ = frames_;
                position_time_ns_ = time_ns;
            }
            if (time_info_) {
                ASIOTime t;
                std::memset(&t, 0, sizeof t);
                t.timeInfo.speed = 1.0;
                split64(time_ns, &t.timeInfo.systemTime.hi, &t.timeInfo.systemTime.lo);
                split64(frames_, &t.timeInfo.samplePosition.hi, &t.timeInfo.samplePosition.lo);
                t.timeInfo.sampleRate = kRate;
                t.timeInfo.flags = kSystemTimeValid | kSamplePositionValid | kSampleRateValid | kSpeedValid;
                callbacks_->bufferSwitchTimeInfo(&t, index, ASIOTrue);
            } else {
                callbacks_->bufferSwitch(index, ASIOTrue);
            }
            // Outputs: filled by host during callback, sent to network.
            std::fill(out_frames_.begin(), out_frames_.end(), 0.0f);
            for (const Buf &b : buffers_) {
                if (b.input) {
                    continue;
                }
                const float *src = b.data.data() + static_cast<size_t>(index) * n;
                for (size_t f = 0; f < n; f++) {
                    out_frames_[f * ch_out + static_cast<size_t>(b.channel)] = src[f];
                }
            }
            lw_ring_write(base_, LW_TO_NET, out_frames_.data(), static_cast<uint32_t>(n));
            index ^= 1;
            next += period;
            frames_ += period;
        }
    }

    // Monitoring (separate connection): device recreated/service stopped → reset.
    void start_monitor() {
        monitor_stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        monitor_thread_ = CreateThread(nullptr, 0, &Driver::monitor_main, this, 0, nullptr);
    }
    void stop_monitor() {
        if (monitor_thread_ != nullptr) {
            SetEvent(monitor_stop_);
            WaitForSingleObject(monitor_thread_, INFINITE);
            CloseHandle(monitor_thread_);
            monitor_thread_ = nullptr;
        }
        if (monitor_stop_ != nullptr) {
            CloseHandle(monitor_stop_);
            monitor_stop_ = nullptr;
        }
    }
    static DWORD WINAPI monitor_main(LPVOID self) {
        static_cast<Driver *>(self)->monitor();
        return 0;
    }
    void monitor() {
        Control c;
        bool requested = false;
        while (WaitForSingleObject(monitor_stop_, 2000) == WAIT_TIMEOUT) {
            Geometry g;
            std::string err;
            bool changed = !read_geometry(c, &g, &err) || g.generation != geom_.generation ||
                           g.to_net != geom_.to_net || g.from_net != geom_.from_net;
            ASIOCallbacks *cb = callbacks_;
            if (changed && !requested && cb != nullptr && cb->asioMessage != nullptr &&
                cb->asioMessage(kAsioSelectorSupported, kAsioResetRequest, nullptr, nullptr) == 1) {
                log_msg(2, err.empty() ? "OpenLW device changed: reset requested"
                                       : "OpenLW service lost (" + err + "): reset requested");
                cb->asioMessage(kAsioResetRequest, 0, nullptr, nullptr);
                requested = true;
            }
        }
    }

    std::atomic<long> refs_{1};
    Control control_;
    Geometry geom_;
    void *handle_ = nullptr;
    void *base_ = nullptr;
    size_t size_ = 0;
    uint64_t qpc_freq_ = 0;

    std::vector<Buf> buffers_;
    std::vector<float> in_frames_, out_frames_;
    long buffer_size_ = 0;
    ASIOCallbacks *volatile callbacks_ = nullptr;
    bool time_info_ = false;

    std::atomic<bool> running_{false};
    HANDLE audio_thread_ = nullptr;
    uint64_t frames_ = 0;
    std::mutex position_mutex_;
    uint64_t position_ = 0, position_time_ns_ = 0;

    HANDLE monitor_thread_ = nullptr;
    HANDLE monitor_stop_ = nullptr;

    std::mutex error_mutex_;
    std::string error_;
};

// ---------- COM factory ----------

class Factory final : public IClassFactory {
public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void **ppv) override {
        if (ppv == nullptr) {
            return E_POINTER;
        }
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IClassFactory)) {
            *ppv = static_cast<IClassFactory *>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return 2; }
    ULONG STDMETHODCALLTYPE Release() override { return 1; }
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown *outer, REFIID riid, void **ppv) override {
        if (outer != nullptr) {
            return CLASS_E_NOAGGREGATION;
        }
        Driver *d = new (std::nothrow) Driver();
        if (d == nullptr) {
            return E_OUTOFMEMORY;
        }
        HRESULT hr = d->QueryInterface(riid, ppv);
        d->Release();
        return hr;
    }
    HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override {
        lock ? g_locks++ : g_locks--;
        return S_OK;
    }
};

Factory g_factory;

std::wstring clsid_string() {
    wchar_t buf[64];
    StringFromGUID2(CLSID_OpenLW, buf, 64);
    return buf;
}

LONG set_value(HKEY root, const std::wstring &path, const wchar_t *name, const std::wstring &value) {
    HKEY k;
    LONG r = RegCreateKeyExW(root, path.c_str(), 0, nullptr, 0, KEY_WRITE, nullptr, &k, nullptr);
    if (r != ERROR_SUCCESS) {
        return r;
    }
    r = RegSetValueExW(k, name, 0, REG_SZ, reinterpret_cast<const BYTE *>(value.c_str()),
                       static_cast<DWORD>((value.size() + 1) * sizeof(wchar_t)));
    RegCloseKey(k);
    return r;
}

} // namespace

extern "C" BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        g_module = instance;
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}

extern "C" HRESULT STDAPICALLTYPE DllGetClassObject(REFCLSID rclsid, REFIID riid, LPVOID *ppv) {
    if (!IsEqualCLSID(rclsid, CLSID_OpenLW)) {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    return g_factory.QueryInterface(riid, ppv);
}

extern "C" HRESULT STDAPICALLTYPE DllCanUnloadNow() {
    return g_objects == 0 && g_locks == 0 ? S_OK : S_FALSE;
}

// Registration (regsvr32, development; installer writes same keys):
// HKCR\CLSID\{…}\InprocServer32 and HKLM\SOFTWARE\ASIO\OpenLW.
extern "C" HRESULT STDAPICALLTYPE DllRegisterServer() {
    wchar_t path[MAX_PATH];
    DWORD n = GetModuleFileNameW(g_module, path, MAX_PATH);
    if (n == 0 || n >= MAX_PATH) {
        return E_FAIL;
    }
    std::wstring clsid = clsid_string();
    std::wstring key = L"CLSID\\" + clsid;
    std::wstring asio = std::wstring(L"SOFTWARE\\ASIO\\") + kDriverName;
    if (set_value(HKEY_CLASSES_ROOT, key, nullptr, kDriverName) != ERROR_SUCCESS ||
        set_value(HKEY_CLASSES_ROOT, key + L"\\InprocServer32", nullptr, path) != ERROR_SUCCESS ||
        set_value(HKEY_CLASSES_ROOT, key + L"\\InprocServer32", L"ThreadingModel", L"Apartment") != ERROR_SUCCESS ||
        set_value(HKEY_LOCAL_MACHINE, asio, L"CLSID", clsid) != ERROR_SUCCESS ||
        set_value(HKEY_LOCAL_MACHINE, asio, L"Description", kDriverName) != ERROR_SUCCESS) {
        return E_FAIL;
    }
    return S_OK;
}

extern "C" HRESULT STDAPICALLTYPE DllUnregisterServer() {
    std::wstring key = L"CLSID\\" + clsid_string();
    RegDeleteTreeW(HKEY_CLASSES_ROOT, key.c_str());
    RegDeleteTreeW(HKEY_LOCAL_MACHINE, (std::wstring(L"SOFTWARE\\ASIO\\") + kDriverName).c_str());
    return S_OK;
}
