/* Included after native.c by the Rust test; access is private to this ABI test. */
#include <assert.h>
#include <dirent.h>
#include <poll.h>
static int fd_count(void) {
    DIR *d = opendir(
#ifdef __APPLE__
        "/dev/fd"
#else
        "/proc/self/fd"
#endif
    ); assert(d); int n = 0;
    while (readdir(d)) ++n;
    closedir(d); return n;
}
int main(void) {
#ifdef __APPLE__
    /* The runtime's one private identity queue lives until exit; count from after it. */
    pthread_once(&net_witness_once, net_witness_open);
#endif
    int baseline = fd_count();
    TarnNetRaw out; tarn_rt_net_poll_new(&out); assert(!out.code);
    TarnPoll *registry = (TarnPoll *)(uintptr_t)out.value;
    TarnEvent events[80]; uint64_t previous = 0;
    int reused = -1;
    for (int i = 0; i < 1024; ++i) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0); assert(fd >= 0);
        if (reused >= 0) assert(fd == reused);
        tarn_rt_net_poll_ctl(&out, registry, fd, 0, 2, 0); assert(!out.code);
        uint64_t token = out.value; assert(token > previous);
        tarn_rt_net_poll_wait(&out, registry, events, 1, 0); assert(!out.code && out.value == 1);
        assert(events[0].token == token && events[0].writable);
        if (previous) {
            tarn_rt_net_poll_ctl(&out, registry, fd, 1, 1, previous); assert(out.code == ENOENT);
            tarn_rt_net_poll_ctl(&out, registry, fd, 2, 1, previous); assert(out.code == ENOENT);
        }
        tarn_rt_net_poll_ctl(&out, registry, fd, 1, 3, token); assert(!out.code);
        if (i & 1) { tarn_rt_net_poll_ctl(&out, registry, fd, 2, 1, token); assert(!out.code); }
        close(fd); /* close while registered on alternate paths */
        tarn_rt_net_poll_wait(&out, registry, events, 1, 0); assert(!out.code && !out.value);
        /* Saved events remain old integer observations, never references. */
        assert(events[0].token == token); previous = token; reused = fd;
    }
    int fds[80]; uint64_t tokens[80]; unsigned char seen[80] = {0};
    for (int i = 0; i < 80; ++i) {
        fds[i] = socket(AF_INET, SOCK_DGRAM, 0); assert(fds[i] >= 0);
        tarn_rt_net_poll_ctl(&out, registry, fds[i], 0, 2, 0); assert(!out.code); tokens[i] = out.value;
    }
    for (int cycle = 0; cycle < 4; ++cycle) {
        tarn_rt_net_poll_wait(&out, registry, events, 80, 0); assert(!out.code && out.value == 64);
        for (int j = 0; j < out.value; ++j) {
            int found = 0;
            for (int k = 0; k < 80; ++k) if (tokens[k] == events[j].token) { seen[k] = 1; found = 1; }
            assert(found);
        }
    }
    for (int i = 0; i < 80; ++i) { assert(seen[i]); close(fds[i]); }
    tarn_rt_net_poll_wait(&out, registry, events, 1, 1); assert(!out.code && out.value == 0);
    /* Saturate an actual nonblocking TCP connection, then drain/retry. */
    int listener = socket(AF_INET, SOCK_STREAM, 0); assert(listener >= 0);
    struct sockaddr_in address = {0}; address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 128) == 0);
    socklen_t size = sizeof(address); assert(getsockname(listener, (struct sockaddr *)&address, &size) == 0);
    for (int i = 0; i < 128; ++i) {
        int client = socket(AF_INET, SOCK_STREAM, 0); assert(client >= 0);
        assert(connect(client, (struct sockaddr *)&address, sizeof(address)) == 0);
        int server = accept(listener, NULL, NULL); assert(server >= 0);
        if (!i) {
            int capacity = 4096;
            assert(setsockopt(client, SOL_SOCKET, SO_SNDBUF, &capacity, sizeof(capacity)) == 0);
            tarn_rt_net_nonblocking(&out, client, 1); assert(!out.code);
            tarn_rt_net_nonblocking(&out, server, 1); assert(!out.code);
            static uint8_t bytes[1048576]; memset(bytes, 42, sizeof(bytes));
            uint64_t written = 0, read = 0; int partial = 0, blocked = 0;
            for (int cycle = 0; cycle < 100000 && written < sizeof(bytes); ++cycle) {
                tarn_rt_net_write(&out, client, bytes + written, sizeof(bytes) - written);
                if (out.code) { assert(out.code == EAGAIN); blocked = 1; break; }
                assert(out.value > 0); partial |= (uint64_t)out.value < sizeof(bytes) - written;
                written += out.value;
            }
            assert(partial && blocked && written < sizeof(bytes));
            for (int cycle = 0; cycle < 100000 && read < sizeof(bytes); ++cycle) {
                struct pollfd ready = { .fd = written > read ? server : client,
                    .events = written > read ? POLLIN : POLLOUT };
                assert(poll(&ready, 1, 5000) > 0);
                tarn_rt_net_read(&out, server, bytes, sizeof(bytes));
                if (!out.code) { assert(out.value > 0); read += out.value; }
                else assert(out.code == EAGAIN);
                if (written < sizeof(bytes)) {
                    tarn_rt_net_write(&out, client, bytes, sizeof(bytes) - written);
                    if (!out.code) { assert(out.value > 0); written += out.value; }
                    else assert(out.code == EAGAIN);
                }
            }
            assert(written == sizeof(bytes) && read == written);
        }
        close(server); close(client);
    }
    close(listener);
    int fd = socket(AF_INET, SOCK_DGRAM, 0); assert(fd >= 0);
    atomic_store(&net_next_token, INT64_MAX);
    tarn_rt_net_poll_ctl(&out, registry, fd, 0, 2, 0); assert(out.code == EOVERFLOW);
    close(fd); tarn_rt_net_poll_drop(registry);
    assert(fd_count() == baseline);
    return 0;
}
