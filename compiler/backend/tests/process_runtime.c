/* Linked into the private runtime test, not a public Tarn API. */
#include <assert.h>
static int descriptors(void) {
    DIR *dir = opendir(
#ifdef __APPLE__
        "/dev/fd"
#else
        "/proc/self/fd"
#endif
    ); assert(dir); int n = 0;
    while (readdir(dir)) ++n;
    closedir(dir); return n;
}
static void spawn_raw(TarnNetRaw *out, const char *program, const char **args, size_t count, int capture) {
    TarnString *text = tarn_rt_string((const unsigned char *)program, strlen(program));
    TarnString **values = calloc(count ? count : 1, sizeof(*values)); assert(values);
    for (size_t i = 0; i < count; ++i) values[i] = tarn_rt_string((const unsigned char *)args[i], strlen(args[i]));
    TarnString *cwd = tarn_rt_string((const unsigned char *)"", 0);
    tarn_rt_process_spawn(out, text, (const TarnString *const *)values, count, NULL, 0, cwd, 0, capture, 0, NULL, 0);
    for (size_t i = 0; i < count; ++i) free(values[i]);
    free(values); free(cwd); free(text);
}
static void reaped(int pid) { int status; errno = 0; assert(waitpid(pid, &status, WNOHANG) == -1 && errno == ECHILD); }
static int descriptor(TarnNetRaw *raw, size_t index) {
    uint8_t *b = (uint8_t *)&raw->address;
    return (int)((uint32_t)b[index] | (uint32_t)b[index+1] << 8 | (uint32_t)b[index+2] << 16 | (uint32_t)b[index+3] << 24);
}
int main(int argc, char **argv) {
    assert(argc == 2);
    int before = descriptors(); TarnNetRaw out;
    /* Even descriptors not marked CLOEXEC must not escape into the child. */
    int extra = open("/dev/null", O_RDONLY); assert(extra >= 3);
    char fd[32]; snprintf(fd, sizeof(fd), "%d", extra);
    const char *args[] = {"fd", fd};
    spawn_raw(&out, argv[1], args, 2, 0); assert(!out.code); int pid = (int)out.value;
    tarn_rt_process_wait(&out, pid); assert(!out.code && out.value == 0); reaped(pid);
    assert(fcntl(extra, F_GETFD) >= 0); close(extra);
    for (int i = 0; i < 64; ++i) {
        spawn_raw(&out, "/usr/bin/true", NULL, 0, 0); assert(!out.code); pid = (int)out.value;
        tarn_rt_process_drop(pid); reaped(pid);
        spawn_raw(&out, "/missing-tarn-15d", NULL, 0, 1); assert(out.domain == 4 && out.code == ENOENT);
        assert(descriptors() == before);
    }
    /* Parent standard fds can be closed. Relocate pipe fds before spawn actions
     * so child dup/close operations cannot collide with their own inputs. */
    int saved0 = dup(0), saved1 = dup(1); assert(saved0 >= 3 && saved1 >= 3);
    close(0); close(1);
    const char *message[] = {"closed-stdio"};
    spawn_raw(&out, "/usr/bin/printf", message, 1, 1); assert(!out.code); pid = (int)out.value;
    int first = descriptor(&out, 0), second = descriptor(&out, 4); assert(first >= 3 && second >= 3);
    uint8_t buffer[32]; tarn_rt_process_read(&out, first, buffer, sizeof(buffer));
    assert(!out.code && out.value == 12 && !memcmp(buffer, "closed-stdio", 12));
    tarn_rt_process_pipe_drop(first); tarn_rt_process_pipe_drop(second);
    tarn_rt_process_wait(&out, pid); assert(!out.code); reaped(pid);
    assert(!setenv("TARN_PROCESS_DUP_FAIL", "1", 1));
    spawn_raw(&out, "/usr/bin/true", NULL, 0, 1); assert(out.domain == 4 && out.code == EMFILE);
    assert(!unsetenv("TARN_PROCESS_DUP_FAIL"));
    assert(dup2(saved0, 0) == 0 && dup2(saved1, 1) == 1); close(saved0); close(saved1);
    assert(descriptors() == before);
    tarn_rt_process_kill(&out, 0); assert(out.code == EINVAL);
    return 0;
}
