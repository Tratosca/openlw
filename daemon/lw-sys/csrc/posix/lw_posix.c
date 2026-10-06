/*
 * Couche C du daemon OpenLW, partie commune à macOS et Linux : appartenance à un groupe, identité du
 * processus à l'autre bout d'une socket Unix.
 */
#if defined(__linux__)
#define _GNU_SOURCE /* struct ucred, getgrouplist */
#endif

#include "../lw_sys.h"

#include <grp.h>
#include <pwd.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <sys/un.h> /* LOCAL_PEERPID */
#endif

int lw_uid_in_group(uint32_t uid, const char *group) {
    if (uid == 0) {
        return 1;
    }
    if (group == NULL) {
        return 0;
    }
    struct passwd pw, *pres = NULL;
    char pwbuf[4096];
    if (getpwuid_r((uid_t)uid, &pw, pwbuf, sizeof pwbuf, &pres) != 0 || pres == NULL) {
        return 0;
    }
    struct group gr, *gres = NULL;
    char grbuf[16384];
    if (getgrnam_r(group, &gr, grbuf, sizeof grbuf, &gres) != 0 || gres == NULL) {
        return 0;
    }
    if (pw.pw_gid == gr.gr_gid) {
        return 1;
    }
#if defined(__APPLE__)
    int groups[256];
    int n = 256;
    if (getgrouplist(pw.pw_name, (int)pw.pw_gid, groups, &n) == -1) {
        n = 256;
    }
#else
    gid_t groups[256];
    int n = 256;
    if (getgrouplist(pw.pw_name, pw.pw_gid, groups, &n) == -1) {
        n = 256;
    }
#endif
    for (int i = 0; i < n && i < 256; i++) {
        if ((gid_t)groups[i] == gr.gr_gid) {
            return 1;
        }
    }
    return 0;
}

int lw_peer_cred(int fd, uint32_t *uid, uint32_t *pid) {
#if defined(__APPLE__)
    uid_t u;
    gid_t g;
    if (getpeereid(fd, &u, &g) != 0) {
        return -1;
    }
    pid_t p = 0;
    socklen_t len = sizeof p;
    if (getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &p, &len) != 0) {
        p = 0;
    }
    *uid = (uint32_t)u;
    *pid = (uint32_t)p;
    return 0;
#else
    struct ucred cred;
    socklen_t len = sizeof cred;
    if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &len) != 0) {
        return -1;
    }
    *uid = (uint32_t)cred.uid;
    *pid = (uint32_t)cred.pid;
    return 0;
#endif
}

uint32_t lw_geteuid(void) {
    return (uint32_t)geteuid();
}
