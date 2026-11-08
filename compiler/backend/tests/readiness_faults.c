#ifdef __APPLE__
#define _DARWIN_C_SOURCE
#include <sys/types.h>
#include <sys/event.h>
#else
#define _POSIX_C_SOURCE 200809L
#include <sys/epoll.h>
#endif
#include <sys/socket.h>
#include <time.h>
#include <errno.h>
#include <stdlib.h>
#include <stdint.h>
#include <assert.h>
#ifdef __APPLE__
int __real_kevent(int, const struct kevent *, int, struct kevent *, int, const struct timespec *);
#else
int __real_epoll_wait(int, struct epoll_event *, int, int);
#endif
int __real_clock_gettime(clockid_t, struct timespec *);
ssize_t __real_send(int, const void *, size_t, int);
int __real_connect(int, const struct sockaddr *, socklen_t);
static int waits, sends;
int __wrap_clock_gettime(clockid_t clock, struct timespec *time) {
    if (getenv("TEST_EINTR")) {
        assert(clock == CLOCK_MONOTONIC);
        time->tv_sec = 10;
        time->tv_nsec = waits * 2000000;
        return 0;
    }
    return __real_clock_gettime(clock, time);
}
#ifdef __APPLE__
int __wrap_kevent(int fd, const struct kevent *changes, int changed, struct kevent *events, int count, const struct timespec *limit) {
    if (getenv("TEST_EINTR") && !changed) {
        assert(count > 0 && limit);
        assert(limit->tv_sec * 1000 + limit->tv_nsec / 1000000 == 5 - waits * 2);
        ++waits;
        if (waits <= 2 || getenv("TEST_EXPIRE")) { errno = EINTR; return -1; }
        return 0;
    }
    return __real_kevent(fd, changes, changed, events, count, limit);
}
#else
int __wrap_epoll_wait(int fd, struct epoll_event *events, int count, int timeout) {
    if (getenv("TEST_EINTR")) {
        assert(count > 0);
        assert(timeout == 5 - waits * 2);
        ++waits;
        if (waits <= 2 || getenv("TEST_EXPIRE")) { errno = EINTR; return -1; }
        return 0;
    }
    return __real_epoll_wait(fd, events, count, timeout);
}
#endif
ssize_t __wrap_send(int fd, const void *bytes, size_t count, int flags) {
    if (getenv("TEST_PARTIAL")) {
        ++sends;
        if (sends % 2 == 0) { errno = EAGAIN; return -1; }
        return __real_send(fd, bytes, count ? 1 : 0, flags);
    }
    return __real_send(fd, bytes, count, flags);
}
int __wrap_connect(int fd, const struct sockaddr *address, socklen_t len) {
    int result = __real_connect(fd, address, len);
    /* Force the immediate-success public branch without inventing a connection:
       await actual loopback completion, then let Tarn receive successful start. */
    if (getenv("TEST_IMMEDIATE") && result < 0 && errno == EINPROGRESS) {
        int error = 0; socklen_t size = sizeof(error);
        for (int i = 0; i < 100000; ++i) {
            struct sockaddr_storage peer; socklen_t n = sizeof(peer);
            if (getpeername(fd, (struct sockaddr *)&peer, &n) == 0) return 0;
            assert(getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) == 0 && error == 0);
        }
        abort();
    }
    return result;
}
