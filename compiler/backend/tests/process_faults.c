/* Deterministic bridge failures; never use sleeps to prove progress. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#ifdef __APPLE__
int __real_pipe(int *);
#else
int __real_pipe2(int *, int);
#endif
int __real_fcntl(int, int, ...);
ssize_t __real_read(int, void *, size_t);
pid_t __real_waitpid(pid_t, int *, int);
int __real_close(int);
#ifdef __APPLE__
#define SPAWN(name) name##posix_spawn
#else
#define SPAWN(name) name##posix_spawnp
#endif
int SPAWN(__real_)(pid_t *, const char *, const posix_spawn_file_actions_t *, const posix_spawnattr_t *, char *const [], char *const []);
static _Atomic unsigned pipe_calls, reads, waits, closes;
#ifdef __APPLE__
int __wrap_pipe(int *fds) {
    unsigned call = atomic_fetch_add(&pipe_calls, 1);
    if (getenv("TARN_PROCESS_PIPE_FAIL") && call == 1) { errno = EMFILE; return -1; }
    return __real_pipe(fds);
}
#else
int __wrap_pipe2(int *fds, int flags) {
    unsigned call = atomic_fetch_add(&pipe_calls, 1);
    if (getenv("TARN_PROCESS_PIPE_FAIL") && call == 1) { errno = EMFILE; return -1; }
    return __real_pipe2(fds, flags);
}
#endif
int __wrap_fcntl(int fd, int command, ...) {
    /* Every other command forwards its argument; those without one ignore it. */
    va_list args; va_start(args, command); int value = va_arg(args, int); va_end(args);
    if (command == F_DUPFD_CLOEXEC && getenv("TARN_PROCESS_DUP_FAIL")) { errno = EMFILE; return -1; }
    return __real_fcntl(fd, command, value);
}
ssize_t __wrap_read(int fd, void *data, size_t count) {
    unsigned call = atomic_fetch_add(&reads, 1);
    if (getenv("TARN_PROCESS_READ_FAIL")) { errno = EIO; return -1; }
    if (getenv("TARN_PROCESS_INTERRUPTS")) {
        if (!call) { errno = EINTR; return -1; }
        if (count > 3) count = 3;
    }
    return __real_read(fd, data, count);
}
pid_t __wrap_waitpid(pid_t pid, int *status, int flags) {
    unsigned call = atomic_fetch_add(&waits, 1);
    if (getenv("TARN_PROCESS_INTERRUPTS") && !call) { errno = EINTR; return -1; }
    return __real_waitpid(pid, status, flags);
}
int __wrap_close(int fd) {
    int result = __real_close(fd);
    if (!result && getenv("TARN_PROCESS_CLOSE_EINTR") && !atomic_fetch_add(&closes, 1)) { errno = EINTR; return -1; }
    return result;
}
int SPAWN(__wrap_)(pid_t *pid, const char *program, const posix_spawn_file_actions_t *actions, const posix_spawnattr_t *attributes, char *const args[], char *const env[]) {
    if (strstr(program, "denied-process")) return EACCES;
    return SPAWN(__real_)(pid, program, actions, attributes, args, env);
}
