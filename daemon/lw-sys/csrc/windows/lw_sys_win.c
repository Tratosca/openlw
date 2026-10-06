/*
 * Couche C du daemon OpenLW, Windows (10 22H2 et plus, x64 et ARM64). Le daemon tourne en service ;
 * les clients (app, pilote ASIO, `lw-daemon ctl`) le joignent par un tube nommé local.
 *
 * Sécurité du tube :
 * - DACL : SYSTEM et Administrateurs en contrôle total ; utilisateurs authentifiés en lecture et
 *   écriture de données seulement, sans FILE_CREATE_PIPE_INSTANCE (pas de prise de nom par un tiers).
 * - Premier exemplaire créé avec FILE_FLAG_FIRST_PIPE_INSTANCE, connexions distantes refusées.
 * - L'appelant est identifié par son jeton (impersonation au niveau Identification) : seuls les
 *   administrateurs en session élevée, LocalSystem et les membres du groupe d'édition modifient.
 *
 * DSCP : Windows ignore IP_TOS ; le marquage passe par qWAVE (QOSSetOutgoingDSCPValue), réservé aux
 * administrateurs et aux services.
 */
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0A00
#endif

#include "../lw_sys.h"

#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <avrt.h>
#include <qos2.h>
#include <sddl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef CREATE_WAITABLE_TIMER_HIGH_RESOLUTION
#define CREATE_WAITABLE_TIMER_HIGH_RESOLUTION 0x00000002
#endif

#if defined(_MSC_VER) && !defined(__clang__)
#define LW_TLS __declspec(thread)
#else
#define LW_TLS _Thread_local
#endif

/* ---------- Conversions UTF-8 ↔ UTF-16 ---------- */

static wchar_t *to_wide(const char *s) {
    if (s == NULL) {
        s = "";
    }
    int n = MultiByteToWideChar(CP_UTF8, 0, s, -1, NULL, 0);
    if (n <= 0) {
        return NULL;
    }
    wchar_t *w = (wchar_t *)malloc((size_t)n * sizeof(wchar_t));
    if (w != NULL) {
        MultiByteToWideChar(CP_UTF8, 0, s, -1, w, n);
    }
    return w;
}

static void to_utf8(const wchar_t *w, char *out, size_t cap) {
    if (cap == 0) {
        return;
    }
    out[0] = '\0';
    if (w != NULL && WideCharToMultiByte(CP_UTF8, 0, w, -1, out, (int)cap, NULL, NULL) == 0) {
        out[0] = '\0';
    }
}

/* ---------- Temps réel ---------- */

int lw_rt_promote(uint64_t period_ns, uint64_t computation_ns, uint64_t constraint_ns) {
    (void)period_ns;
    (void)computation_ns;
    (void)constraint_ns;
    DWORD task = 0;
    HANDLE h = AvSetMmThreadCharacteristicsW(L"Pro Audio", &task);
    if (h == NULL) {
        return (int)GetLastError();
    }
    if (!AvSetMmThreadPriority(h, AVRT_PRIORITY_HIGH)) {
        return (int)GetLastError();
    }
    /* Le handle MMCSS reste associé au thread jusqu'à sa fin. */
    return 0;
}

void lw_sleep_ns(uint64_t ns) {
    static LW_TLS HANDLE timer = NULL;
    if (timer == NULL) {
        timer = CreateWaitableTimerExW(NULL, NULL, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
        if (timer == NULL) { /* systèmes sans minuteur haute résolution */
            timer = CreateWaitableTimerExW(NULL, NULL, 0, TIMER_ALL_ACCESS);
        }
    }
    LARGE_INTEGER due;
    due.QuadPart = -(LONGLONG)((ns + 99) / 100); /* relatif, en unités de 100 ns */
    if (timer == NULL || !SetWaitableTimer(timer, &due, 0, NULL, NULL, FALSE)) {
        Sleep((DWORD)((ns + 999999) / 1000000));
        return;
    }
    WaitForSingleObject(timer, INFINITE);
}

/* ---------- Journal ---------- */

void lw_log(int level, const char *category, const char *message) {
    char line[2048];
    snprintf(line, sizeof line, "OpenLW [%s] %s\n", category ? category : "daemon", message ? message : "");
    wchar_t *w = to_wide(line);
    if (w == NULL) {
        return;
    }
    OutputDebugStringW(w);
    if (level >= 2) {
        /* Source « OpenLW » déclarée par l'installeur ; sans elle, l'événement reste lisible. */
        static HANDLE source = NULL;
        if (source == NULL) {
            source = RegisterEventSourceW(NULL, L"OpenLW");
        }
        if (source != NULL) {
            LPCWSTR strings[1] = {w};
            WORD type = level >= 3 ? EVENTLOG_ERROR_TYPE : EVENTLOG_INFORMATION_TYPE;
            ReportEventW(source, type, 0, 1, NULL, 1, 0, strings, NULL);
        }
    }
    free(w);
}

/* ---------- Horloge hôte : QueryPerformanceCounter ---------- */

static uint64_t qpc_frequency(void) {
    static uint64_t freq = 0;
    if (freq == 0) {
        LARGE_INTEGER f;
        QueryPerformanceFrequency(&f);
        freq = f.QuadPart > 0 ? (uint64_t)f.QuadPart : 10000000u;
    }
    return freq;
}

uint64_t lw_host_time(void) {
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (uint64_t)t.QuadPart;
}

uint64_t lw_host_time_to_ns(uint64_t t) {
    uint64_t f = qpc_frequency();
    return (t / f) * 1000000000ull + (t % f) * 1000000000ull / f;
}

void lw_host_clock_info(lw_host_clock *clock) {
    clock->id = LW_CLOCK_QPC;
    clock->ns_numer = 1000000000ull;
    clock->ns_denom = qpc_frequency();
}

/* ---------- Région partagée : section anonyme, dupliquée vers chaque client ---------- */

void *lw_shm_alloc(size_t size, void **handle) {
    *handle = NULL;
    uint64_t sz = (uint64_t)size;
    HANDLE h = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, (DWORD)(sz >> 32), (DWORD)sz, NULL);
    if (h == NULL) {
        return NULL;
    }
    void *base = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, size);
    if (base == NULL) {
        CloseHandle(h);
        return NULL;
    }
    *handle = h;
    return base; /* pages d'une section neuve : déjà à zéro */
}

void *lw_shm_map(void *handle, size_t *size) {
    *size = 0;
    if (handle == NULL) {
        return NULL;
    }
    void *base = MapViewOfFile((HANDLE)handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
    if (base == NULL) {
        return NULL;
    }
    MEMORY_BASIC_INFORMATION mbi;
    if (VirtualQuery(base, &mbi, sizeof mbi) == 0) {
        UnmapViewOfFile(base);
        return NULL;
    }
    *size = mbi.RegionSize;
    return base;
}

void lw_shm_unmap(void *base, size_t size) {
    (void)size;
    if (base != NULL) {
        UnmapViewOfFile(base);
    }
}

void lw_shm_release(void *handle) {
    if (handle != NULL) {
        CloseHandle((HANDLE)handle);
    }
}

uint64_t lw_shm_share_with(void *handle, uint32_t pid) {
    if (handle == NULL || pid == 0) {
        return 0;
    }
    HANDLE proc = OpenProcess(PROCESS_DUP_HANDLE, FALSE, pid);
    if (proc == NULL) {
        return 0;
    }
    HANDLE out = NULL;
    BOOL ok = DuplicateHandle(GetCurrentProcess(), (HANDLE)handle, proc, &out, FILE_MAP_READ | FILE_MAP_WRITE, FALSE, 0);
    CloseHandle(proc);
    return ok ? (uint64_t)(uintptr_t)out : 0;
}

void lw_free(char *p) {
    free(p);
}

/* ---------- DSCP par qWAVE ---------- */

typedef struct {
    HANDLE qos;
    QOS_FLOWID flow;
    SOCKET sock;
} lw_qos_flow;

void *lw_qos_dscp(uint64_t sock, const uint8_t dest_ip[4], uint16_t dest_port, uint32_t dscp, int *error) {
    *error = 0;
    QOS_VERSION v = {1, 0};
    HANDLE h = NULL;
    if (!QOSCreateHandle(&v, &h)) {
        *error = (int)GetLastError();
        return NULL;
    }
    struct sockaddr_in dest;
    memset(&dest, 0, sizeof dest);
    dest.sin_family = AF_INET;
    dest.sin_port = htons(dest_port);
    memcpy(&dest.sin_addr, dest_ip, 4);
    QOS_FLOWID flow = 0;
    if (!QOSAddSocketToFlow(h, (SOCKET)sock, (struct sockaddr *)&dest, QOSTrafficTypeAudioVideo,
                            QOS_NON_ADAPTIVE_FLOW, &flow)) {
        *error = (int)GetLastError();
        QOSCloseHandle(h);
        return NULL;
    }
    DWORD value = dscp;
    if (!QOSSetFlow(h, flow, QOSSetOutgoingDSCPValue, sizeof value, &value, 0, NULL)) {
        *error = (int)GetLastError();
        QOSRemoveSocketFromFlow(h, (SOCKET)sock, flow, 0);
        QOSCloseHandle(h);
        return NULL;
    }
    lw_qos_flow *f = (lw_qos_flow *)malloc(sizeof *f);
    if (f == NULL) {
        QOSRemoveSocketFromFlow(h, (SOCKET)sock, flow, 0);
        QOSCloseHandle(h);
        *error = ERROR_NOT_ENOUGH_MEMORY;
        return NULL;
    }
    f->qos = h;
    f->flow = flow;
    f->sock = (SOCKET)sock;
    return f;
}

void lw_qos_close(void *flow) {
    lw_qos_flow *f = (lw_qos_flow *)flow;
    if (f == NULL) {
        return;
    }
    QOSRemoveSocketFromFlow(f->qos, f->sock, f->flow, 0);
    QOSCloseHandle(f->qos);
    free(f);
}

/* ---------- Tube nommé : serveur ---------- */

#define PIPE_BUF_BYTES 65536u
#define MAX_REQUEST_BYTES (1u << 20)
/* SYSTEM et Administrateurs : GA ; utilisateurs authentifiés : FILE_GENERIC_READ | FILE_WRITE_DATA |
 * FILE_WRITE_ATTRIBUTES (0x12018b), sans FILE_APPEND_DATA (= FILE_CREATE_PIPE_INSTANCE). */
#define PIPE_SDDL L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12018b;;;AU)"

struct lw_pipe_server {
    wchar_t *path;
    lw_pipe_handler_fn handler;
    lw_pipe_free_fn free_response;
    void *ctx;
    PSID edit_sid; /* groupe d'édition, ou NULL */
    PSECURITY_DESCRIPTOR sd;
    SECURITY_ATTRIBUTES sa;
    HANDLE stop;       /* événement manuel : arrêt demandé */
    HANDLE idle;       /* événement manuel : aucune connexion active */
    HANDLE first;      /* premier exemplaire, créé au démarrage */
    HANDLE accept_thread;
    volatile LONG active;
};

typedef struct {
    struct lw_pipe_server *server;
    HANDLE pipe;
} pipe_conn;

static HANDLE create_instance(struct lw_pipe_server *s, BOOL first) {
    DWORD open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | (first ? FILE_FLAG_FIRST_PIPE_INSTANCE : 0);
    DWORD mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    return CreateNamedPipeW(s->path, open, mode, PIPE_UNLIMITED_INSTANCES, PIPE_BUF_BYTES, PIPE_BUF_BYTES, 0, &s->sa);
}

static PSID lookup_group(const char *name) {
    if (name == NULL || name[0] == '\0') {
        return NULL;
    }
    wchar_t *w = to_wide(name);
    if (w == NULL) {
        return NULL;
    }
    DWORD sid_len = 0, dom_len = 0;
    SID_NAME_USE use;
    LookupAccountNameW(NULL, w, NULL, &sid_len, NULL, &dom_len, &use);
    PSID sid = NULL;
    wchar_t *dom = NULL;
    if (GetLastError() == ERROR_INSUFFICIENT_BUFFER && sid_len > 0) {
        sid = malloc(sid_len);
        dom = (wchar_t *)malloc((size_t)(dom_len + 1) * sizeof(wchar_t));
        if (sid == NULL || dom == NULL || !LookupAccountNameW(NULL, w, sid, &sid_len, dom, &dom_len, &use) ||
            (use != SidTypeAlias && use != SidTypeGroup && use != SidTypeWellKnownGroup)) {
            free(sid);
            sid = NULL;
        }
    }
    free(dom);
    free(w);
    return sid;
}

static BOOL token_has(HANDLE token, WELL_KNOWN_SID_TYPE type) {
    BYTE buf[SECURITY_MAX_SID_SIZE];
    DWORD len = sizeof buf;
    BOOL member = FALSE;
    if (!CreateWellKnownSid(type, NULL, buf, &len)) {
        return FALSE;
    }
    return CheckTokenMembership(token, buf, &member) && member;
}

static void identify(struct lw_pipe_server *s, HANDLE pipe, lw_caller *c) {
    memset(c, 0, sizeof *c);
    snprintf(c->user, sizeof c->user, "?");
    ULONG pid = 0;
    if (GetNamedPipeClientProcessId(pipe, &pid)) {
        c->pid = (uint32_t)pid;
    }
    if (!ImpersonateNamedPipeClient(pipe)) {
        return;
    }
    HANDLE token = NULL;
    BOOL got = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &token);
    RevertToSelf();
    if (!got) {
        return;
    }
    BOOL member = FALSE;
    c->may_edit = token_has(token, WinBuiltinAdministratorsSid) || token_has(token, WinLocalSystemSid) ||
                  (s->edit_sid != NULL && CheckTokenMembership(token, s->edit_sid, &member) && member);
    BYTE ubuf[512];
    DWORD ulen = 0;
    if (GetTokenInformation(token, TokenUser, ubuf, sizeof ubuf, &ulen)) {
        wchar_t name[128], dom[128];
        DWORD nlen = 128, dlen = 128;
        SID_NAME_USE use;
        if (LookupAccountSidW(NULL, ((TOKEN_USER *)ubuf)->User.Sid, name, &nlen, dom, &dlen, &use)) {
            wchar_t full[260];
            swprintf(full, 260, L"%ls\\%ls", dom, name);
            full[259] = L'\0';
            to_utf8(full, c->user, sizeof c->user);
        }
    }
    CloseHandle(token);
}

/* Attend la fin d'une E/S recouvrante ou l'arrêt. Retourne 1 si terminée (octets dans *n), 0 sinon. */
static int wait_io(struct lw_pipe_server *s, HANDLE pipe, OVERLAPPED *ov, BOOL started, DWORD *n) {
    if (!started && GetLastError() != ERROR_IO_PENDING) {
        return 0;
    }
    HANDLE events[2] = {ov->hEvent, s->stop};
    DWORD w = WaitForMultipleObjects(2, events, FALSE, INFINITE);
    if (w != WAIT_OBJECT_0) {
        CancelIoEx(pipe, ov);
        GetOverlappedResult(pipe, ov, n, TRUE);
        return 0;
    }
    return GetOverlappedResult(pipe, ov, n, FALSE) ? 1 : 0;
}

static int write_all(struct lw_pipe_server *s, HANDLE pipe, OVERLAPPED *ov, const char *data, size_t len) {
    while (len > 0) {
        DWORD chunk = len > PIPE_BUF_BYTES ? PIPE_BUF_BYTES : (DWORD)len, n = 0;
        ResetEvent(ov->hEvent);
        BOOL ok = WriteFile(pipe, data, chunk, NULL, ov);
        if (!wait_io(s, pipe, ov, ok, &n) || n == 0) {
            return 0;
        }
        data += n;
        len -= n;
    }
    return 1;
}

static DWORD WINAPI conn_main(LPVOID arg) {
    pipe_conn *pc = (pipe_conn *)arg;
    struct lw_pipe_server *s = pc->server;
    HANDLE pipe = pc->pipe;
    free(pc);
    lw_caller caller;
    identify(s, pipe, &caller);
    OVERLAPPED ov;
    memset(&ov, 0, sizeof ov);
    ov.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    size_t cap = 4096, len = 0;
    char *buf = (char *)malloc(cap);
    while (ov.hEvent != NULL && buf != NULL) {
        if (cap - len < 2048) {
            if (cap >= MAX_REQUEST_BYTES) {
                break; /* requête démesurée : connexion fermée */
            }
            char *bigger = (char *)realloc(buf, cap * 2);
            if (bigger == NULL) {
                break;
            }
            buf = bigger;
            cap *= 2;
        }
        DWORD n = 0;
        ResetEvent(ov.hEvent);
        BOOL ok = ReadFile(pipe, buf + len, (DWORD)(cap - len - 1), NULL, &ov);
        if (!wait_io(s, pipe, &ov, ok, &n) || n == 0) {
            break; /* fin de connexion, erreur ou arrêt */
        }
        len += n;
        int alive = 1;
        char *nl;
        while (alive && (nl = (char *)memchr(buf, '\n', len)) != NULL) {
            *nl = '\0';
            if (nl > buf && nl[-1] == '\r') {
                nl[-1] = '\0';
            }
            char *resp = s->handler(buf, &caller, s->ctx);
            const char *out = resp ? resp : "{\"ok\":false,\"error\":\"réponse vide\"}";
            alive = write_all(s, pipe, &ov, out, strlen(out)) && write_all(s, pipe, &ov, "\n", 1);
            if (resp) {
                s->free_response(resp);
            }
            size_t used = (size_t)(nl - buf) + 1;
            memmove(buf, buf + used, len - used);
            len -= used;
        }
        if (!alive) {
            break;
        }
    }
    free(buf);
    if (ov.hEvent != NULL) {
        CloseHandle(ov.hEvent);
    }
    FlushFileBuffers(pipe);
    DisconnectNamedPipe(pipe);
    CloseHandle(pipe);
    if (InterlockedDecrement(&s->active) == 0) {
        SetEvent(s->idle);
    }
    return 0;
}

static DWORD WINAPI accept_main(LPVOID arg) {
    struct lw_pipe_server *s = (struct lw_pipe_server *)arg;
    HANDLE pipe = s->first;
    s->first = NULL;
    OVERLAPPED ov;
    memset(&ov, 0, sizeof ov);
    ov.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    while (ov.hEvent != NULL && WaitForSingleObject(s->stop, 0) != WAIT_OBJECT_0) {
        if (pipe == INVALID_HANDLE_VALUE || pipe == NULL) {
            pipe = create_instance(s, FALSE);
            if (pipe == INVALID_HANDLE_VALUE) {
                WaitForSingleObject(s->stop, 100);
                continue;
            }
        }
        ResetEvent(ov.hEvent);
        BOOL connected = ConnectNamedPipe(pipe, &ov);
        DWORD n = 0;
        if (!connected) {
            DWORD err = GetLastError();
            if (err == ERROR_PIPE_CONNECTED) {
                connected = TRUE;
            } else if (err == ERROR_IO_PENDING) {
                connected = wait_io(s, pipe, &ov, FALSE, &n);
            }
        }
        if (!connected) {
            CloseHandle(pipe);
            pipe = NULL;
            continue;
        }
        pipe_conn *pc = (pipe_conn *)malloc(sizeof *pc);
        if (pc == NULL) {
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
            pipe = NULL;
            continue;
        }
        pc->server = s;
        pc->pipe = pipe;
        if (InterlockedIncrement(&s->active) == 1) {
            ResetEvent(s->idle);
        }
        HANDLE t = CreateThread(NULL, 0, conn_main, pc, 0, NULL);
        if (t == NULL) {
            free(pc);
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
            if (InterlockedDecrement(&s->active) == 0) {
                SetEvent(s->idle);
            }
        } else {
            CloseHandle(t);
        }
        pipe = NULL;
    }
    if (pipe != NULL && pipe != INVALID_HANDLE_VALUE) {
        CloseHandle(pipe);
    }
    if (ov.hEvent != NULL) {
        CloseHandle(ov.hEvent);
    }
    return 0;
}

static void server_free(struct lw_pipe_server *s) {
    if (s->first != NULL && s->first != INVALID_HANDLE_VALUE) {
        CloseHandle(s->first);
    }
    if (s->stop) {
        CloseHandle(s->stop);
    }
    if (s->idle) {
        CloseHandle(s->idle);
    }
    if (s->sd) {
        LocalFree(s->sd);
    }
    free(s->edit_sid);
    free(s->path);
    free(s);
}

static wchar_t *pipe_path(const char *name) {
    char full[512];
    snprintf(full, sizeof full, "\\\\.\\pipe\\%s", name ? name : "");
    return to_wide(full);
}

lw_pipe_server *lw_pipe_server_start(const char *name, const char *edit_group, lw_pipe_handler_fn handler,
                                     lw_pipe_free_fn free_response, void *ctx) {
    struct lw_pipe_server *s = (struct lw_pipe_server *)calloc(1, sizeof *s);
    if (s == NULL) {
        return NULL;
    }
    s->handler = handler;
    s->free_response = free_response;
    s->ctx = ctx;
    s->path = pipe_path(name);
    s->edit_sid = lookup_group(edit_group);
    s->stop = CreateEventW(NULL, TRUE, FALSE, NULL);
    s->idle = CreateEventW(NULL, TRUE, TRUE, NULL);
    if (s->path == NULL || s->stop == NULL || s->idle == NULL ||
        !ConvertStringSecurityDescriptorToSecurityDescriptorW(PIPE_SDDL, SDDL_REVISION_1, &s->sd, NULL)) {
        server_free(s);
        return NULL;
    }
    s->sa.nLength = sizeof s->sa;
    s->sa.lpSecurityDescriptor = s->sd;
    s->sa.bInheritHandle = FALSE;
    /* Nom déjà servi ? Vérification sans consommer d'instance (en plus de FILE_FLAG_FIRST_PIPE_INSTANCE,
     * que tous les environnements n'appliquent pas). */
    if (WaitNamedPipeW(s->path, NMPWAIT_NOWAIT) || GetLastError() != ERROR_FILE_NOT_FOUND) {
        server_free(s);
        return NULL;
    }
    s->first = create_instance(s, TRUE);
    if (s->first == INVALID_HANDLE_VALUE) {
        s->first = NULL;
        server_free(s);
        return NULL;
    }
    s->accept_thread = CreateThread(NULL, 0, accept_main, s, 0, NULL);
    if (s->accept_thread == NULL) {
        server_free(s);
        return NULL;
    }
    return s;
}

void lw_pipe_server_stop(lw_pipe_server *s) {
    if (s == NULL) {
        return;
    }
    SetEvent(s->stop);
    WaitForSingleObject(s->accept_thread, INFINITE);
    CloseHandle(s->accept_thread);
    WaitForSingleObject(s->idle, INFINITE);
    server_free(s);
}

/* ---------- Tube nommé : client ---------- */

struct lw_pipe_client {
    HANDLE pipe;
    char *buf;
    size_t len, cap;
};

lw_pipe_client *lw_pipe_client_connect(const char *name, uint32_t timeout_ms, const char **error) {
    *error = NULL;
    wchar_t *path = pipe_path(name);
    if (path == NULL) {
        *error = "nom de tube invalide";
        return NULL;
    }
    /* Droits limités à ceux que la DACL accorde aux utilisateurs ; le serveur ne peut qu'identifier
     * le client, pas agir en son nom. */
    DWORD access = GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES;
    DWORD flags = SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    HANDLE h = INVALID_HANDLE_VALUE;
    DWORD err = 0;
    ULONGLONG deadline = GetTickCount64() + timeout_ms;
    /* Toutes les instances occupées : on attend la suivante, plusieurs clients peuvent se la disputer. */
    for (;;) {
        h = CreateFileW(path, access, 0, NULL, OPEN_EXISTING, flags, NULL);
        err = GetLastError();
        if (h != INVALID_HANDLE_VALUE || err != ERROR_PIPE_BUSY) {
            break;
        }
        ULONGLONG now = GetTickCount64();
        if (now >= deadline) {
            break;
        }
        WaitNamedPipeW(path, (DWORD)(deadline - now));
    }
    free(path);
    if (h == INVALID_HANDLE_VALUE) {
        *error = err == ERROR_FILE_NOT_FOUND    ? "service OpenLW introuvable (tube absent)"
                 : err == ERROR_ACCESS_DENIED   ? "accès au service OpenLW refusé"
                 : err == ERROR_PIPE_BUSY       ? "service OpenLW occupé"
                                                : "connexion au service OpenLW impossible";
        return NULL;
    }
    struct lw_pipe_client *c = (struct lw_pipe_client *)calloc(1, sizeof *c);
    if (c == NULL) {
        CloseHandle(h);
        *error = "mémoire insuffisante";
        return NULL;
    }
    c->pipe = h;
    return c;
}

char *lw_pipe_call(lw_pipe_client *c, const char *request, const char **error) {
    *error = NULL;
    if (c == NULL || request == NULL || strchr(request, '\n') != NULL) {
        *error = "requête invalide";
        return NULL;
    }
    size_t rlen = strlen(request);
    const char *parts[2] = {request, "\n"};
    size_t lens[2] = {rlen, 1};
    for (int i = 0; i < 2; i++) {
        const char *p = parts[i];
        size_t left = lens[i];
        while (left > 0) {
            DWORD n = 0;
            if (!WriteFile(c->pipe, p, left > PIPE_BUF_BYTES ? PIPE_BUF_BYTES : (DWORD)left, &n, NULL) || n == 0) {
                *error = "connexion au service OpenLW interrompue";
                return NULL;
            }
            p += n;
            left -= n;
        }
    }
    for (;;) {
        char *nl = c->buf ? (char *)memchr(c->buf, '\n', c->len) : NULL;
        if (nl != NULL) {
            size_t line = (size_t)(nl - c->buf);
            char *out = (char *)malloc(line + 1);
            if (out == NULL) {
                *error = "mémoire insuffisante";
                return NULL;
            }
            memcpy(out, c->buf, line);
            out[line] = '\0';
            if (line > 0 && out[line - 1] == '\r') {
                out[line - 1] = '\0';
            }
            memmove(c->buf, nl + 1, c->len - line - 1);
            c->len -= line + 1;
            return out;
        }
        if (c->cap - c->len < 4096) {
            size_t ncap = c->cap ? c->cap * 2 : 8192;
            if (ncap > 64u * MAX_REQUEST_BYTES) {
                *error = "réponse démesurée";
                return NULL;
            }
            char *bigger = (char *)realloc(c->buf, ncap);
            if (bigger == NULL) {
                *error = "mémoire insuffisante";
                return NULL;
            }
            c->buf = bigger;
            c->cap = ncap;
        }
        DWORD n = 0;
        if (!ReadFile(c->pipe, c->buf + c->len, (DWORD)(c->cap - c->len), &n, NULL) || n == 0) {
            *error = "connexion au service OpenLW interrompue";
            return NULL;
        }
        c->len += n;
    }
}

void lw_pipe_client_close(lw_pipe_client *c) {
    if (c == NULL) {
        return;
    }
    CloseHandle(c->pipe);
    free(c->buf);
    free(c);
}
