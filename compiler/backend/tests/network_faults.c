#define _POSIX_C_SOURCE 200809L
#ifdef __APPLE__
#define _DARWIN_C_SOURCE /* SO_NOSIGPIPE */
#endif
#include <sys/socket.h>
#include <netdb.h>
#include <errno.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdatomic.h>
#define ONCE() static atomic_int once; if (getenv("TEST_EINTR") && !atomic_exchange(&once, 1)) { errno = EINTR; return -1; }
int __real_socket(int, int, int);
int __wrap_socket(int a, int b, int c) { ONCE(); return __real_socket(a,b,c); }
int __real_connect(int, const struct sockaddr*, socklen_t);
int __wrap_connect(int fd, const struct sockaddr *a, socklen_t n) { ONCE(); return __real_connect(fd,a,n); }
int __real_accept(int, struct sockaddr*, socklen_t*);
int __wrap_accept(int fd, struct sockaddr *a, socklen_t *n) { ONCE(); return __real_accept(fd,a,n); }
int __real_shutdown(int, int);
int __wrap_shutdown(int fd, int how) { ONCE(); return __real_shutdown(fd,how); }
ssize_t __real_send(int, const void*, size_t, int);
ssize_t __wrap_send(int fd, const void *b, size_t n, int flags) {
    ONCE();
    if (getenv("TEST_ZERO")) return 0;
    if (getenv("TEST_DROP_NOSIGNAL")) {
        flags = 0;
#ifdef __APPLE__
        /* The macOS runtime protects each socket with SO_NOSIGPIPE instead. */
        int off = 0; setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &off, sizeof(off));
#endif
    }
    if (getenv("TEST_PARTIAL") && n > 1) n = 1;
    return __real_send(fd,b,n,flags);
}
ssize_t __real_recv(int, void*, size_t, int);
ssize_t __wrap_recv(int fd, void *b, size_t n, int flags) { ONCE(); return __real_recv(fd,b,n,flags); }
ssize_t __real_recvfrom(int, void*, size_t, int, struct sockaddr*, socklen_t*);
ssize_t __wrap_recvfrom(int fd, void *b, size_t n, int flags, struct sockaddr *a, socklen_t *an) { ONCE(); return __real_recvfrom(fd,b,n,flags,a,an); }
ssize_t __real_sendto(int, const void*, size_t, int, const struct sockaddr*, socklen_t);
ssize_t __wrap_sendto(int fd, const void *b, size_t n, int flags, const struct sockaddr *a, socklen_t an) { ONCE(); return __real_sendto(fd,b,n,flags,a,an); }
int __real_getaddrinfo(const char*, const char*, const struct addrinfo*, struct addrinfo**);
int __wrap_getaddrinfo(const char *host, const char *port, const struct addrinfo *hints, struct addrinfo **result) {
    static atomic_int once;
    if (getenv("TEST_DNS")) return EAI_NONAME;
    if (getenv("TEST_EINTR") && !atomic_exchange(&once, 1)) { errno = EINTR; return EAI_SYSTEM; }
    return __real_getaddrinfo(host,port,hints,result);
}
int __real_bind(int, const struct sockaddr*, socklen_t);
int __wrap_bind(int fd, const struct sockaddr *a, socklen_t n) {
    const char *code = getenv("TEST_ERRNO");
    if (code) {
        errno = atoi(code);
#ifdef __APPLE__
        /* Injected values are canonical (Linux) numbers; raise the native errno. */
        switch (errno) {
            case 98: errno = EADDRINUSE; break; case 111: errno = ECONNREFUSED; break;
            case 104: errno = ECONNRESET; break; case 110: errno = ETIMEDOUT; break;
            case 11: errno = EAGAIN; break;
        }
#endif
        return -1;
    }
    ONCE(); return __real_bind(fd,a,n);
}
int __real_close(int);
int __wrap_close(int fd) {
    static atomic_int once;
    int result = __real_close(fd);
    if (getenv("TEST_CLOSE_EINTR") && result == 0 && !atomic_exchange(&once, 1)) { errno = EINTR; return -1; }
    return result;
}

#include <stdio.h>
#include <arpa/inet.h>
int __real_listen(int, int);
int __wrap_listen(int fd, int backlog) {
    ONCE(); int result = __real_listen(fd,backlog);
    if (result == 0 && getenv("TEST_READY")) {
        struct sockaddr_in a; socklen_t n = sizeof(a); int rc;
        do { rc = getsockname(fd, (struct sockaddr *)&a, &n); } while (rc < 0 && errno == EINTR);
        if (rc != 0) abort();
        fprintf(stderr, "test:ready:%u\n", (unsigned)ntohs(a.sin_port)); fflush(stderr);
    }
    return result;
}

#include <signal.h>
/* Rust's test parent can ignore SIGPIPE. Test the shipped strategy with the
 * default fatal disposition restored in the native child. */
#include <dirent.h>
static int baseline_fds = -1;
static int count_fds(void) {
    DIR *dir = opendir(
#ifdef __APPLE__
        "/dev/fd"
#else
        "/proc/self/fd"
#endif
    ); if (!dir) abort();
    int own = dirfd(dir), count = 0; struct dirent *entry;
    while ((entry = readdir(dir))) {
        if (entry->d_name[0] >= '0' && entry->d_name[0] <= '9' && atoi(entry->d_name) != own) count++;
    }
    closedir(dir); return count;
}
__attribute__((constructor)) static void reset_sigpipe(void) {
    signal(SIGPIPE, SIG_DFL);
    if (getenv("TEST_FD_AUDIT")) {
#ifdef __APPLE__
        /* libSystem's resolver keeps its descriptors for the process after first
         * use; open them before counting so every runtime descriptor still counts. */
        struct addrinfo *found = NULL;
        if (__real_getaddrinfo("localhost", "1", NULL, &found) == 0) freeaddrinfo(found);
#endif
        baseline_fds = count_fds();
    }
}
__attribute__((destructor)) static void audit_fds(void) {
    if (baseline_fds >= 0 && count_fds() != baseline_fds) { fputs("test:fd-leak\n", stderr); abort(); }
}

int __real_getsockname(int, struct sockaddr*, socklen_t*);
// TEST_FOREIGN_FAMILY reports a non-IP family, as an AF_UNIX descriptor would.
int __wrap_getsockname(int fd, struct sockaddr *a, socklen_t *n) {
    ONCE();
    int rc = __real_getsockname(fd,a,n);
    if (rc == 0 && getenv("TEST_FOREIGN_FAMILY")) a->sa_family = AF_UNIX;
    return rc;
}
int __real_getpeername(int, struct sockaddr*, socklen_t*);
int __wrap_getpeername(int fd, struct sockaddr *a, socklen_t *n) {
    ONCE();
    int rc = __real_getpeername(fd,a,n);
    if (rc == 0 && getenv("TEST_FOREIGN_FAMILY")) a->sa_family = AF_UNIX;
    return rc;
}
