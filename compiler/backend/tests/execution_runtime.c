#include <assert.h>
#include <dirent.h>
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
int main(void) {
#ifdef __APPLE__
    /* The runtime's one private identity queue lives until exit; count from after it. */
    pthread_once(&net_witness_once, net_witness_open);
#endif
    int before = descriptors(); TarnNetRaw out;
    tarn_rt_net_poll_new(&out); assert(!out.code); TarnPoll *poll = (TarnPoll *)(uintptr_t)out.value;
    tarn_rt_net_exec_new(&out, poll); assert(!out.code); TarnExecution *owner = (TarnExecution *)(uintptr_t)out.value;
    uint64_t previous = 0;
    for (int i = 0; i < 2048; ++i) {
        uintptr_t slot; tarn_rt_net_waker_new(&slot, owner); TarnWake *wake = (TarnWake *)slot;
        assert(wake->identity > previous); previous = wake->identity;
        tarn_rt_net_wake_take(&out, wake); assert(out.value == 1);
        tarn_rt_net_wake_take(&out, wake); assert(out.value == 0);
        for (int k = 0; k < 128; ++k) tarn_rt_net_wake(&out, wake);
        tarn_rt_net_wake_take(&out, wake); assert(out.value == 1);
        tarn_rt_net_wake_take(&out, wake); assert(out.value == 0);
        int fd = socket(AF_INET, SOCK_DGRAM, 0); assert(fd >= 0);
        tarn_rt_net_wake_arm(&out, wake, fd, 2); assert(!out.code);
        tarn_rt_net_exec_wait(&out, owner, 0); assert(!out.code);
        tarn_rt_net_wake_take(&out, wake); assert(out.value == 1);
        if (i & 1) {
            close(fd);
            int replacement = socket(AF_INET, SOCK_DGRAM, 0); assert(replacement == fd);
            tarn_rt_net_wake_clear(&out, wake);
            tarn_rt_net_poll_ctl(&out, poll, replacement, 0, 2, 0); assert(!out.code);
            uint64_t token = out.value;
            tarn_rt_net_waker_drop(wake);
            tarn_rt_net_poll_ctl(&out, poll, replacement, 2, 2, token); assert(!out.code);
            close(replacement);
        } else { tarn_rt_net_waker_drop(wake); close(fd); }
        tarn_rt_net_exec_wait(&out, owner, 0); assert(!out.code && !out.value);
        assert(!owner->head && !poll->head);
    }
    uintptr_t p, c; tarn_rt_net_waker_new(&p, owner); tarn_rt_net_waker_new(&c, owner);
    TarnWake *parent = (TarnWake *)p, *child = (TarnWake *)c;
    tarn_rt_net_wake_link(&out, child, parent); assert(!out.code);
    tarn_rt_net_wake_link(&out, parent, child); assert(out.code == EINVAL);
    tarn_rt_net_wake_take(&out, parent);
    tarn_rt_net_wake(&out, child); tarn_rt_net_wake_take(&out, parent); assert(out.value == 1);
    tarn_rt_net_waker_drop(parent); /* Stale forwarding identity is inert. */
    tarn_rt_net_wake(&out, child); tarn_rt_net_waker_drop(child);
    TarnWake *wakes[64]; int fds[64];
    for (int i = 0; i < 64; ++i) {
        uintptr_t slot; tarn_rt_net_waker_new(&slot, owner); wakes[i] = (TarnWake *)slot;
        tarn_rt_net_wake_take(&out, wakes[i]);
        fds[i] = socket(AF_INET, SOCK_DGRAM, 0); assert(fds[i] >= 0);
        tarn_rt_net_wake_arm(&out, wakes[i], fds[i], 2); assert(!out.code);
    }
    tarn_rt_net_exec_wait(&out, owner, 0); assert(!out.code && out.value == 64);
    for (int i = 0; i < 64; ++i) {
        tarn_rt_net_wake_take(&out, wakes[i]); assert(out.value == 1);
        tarn_rt_net_waker_drop(wakes[i]); close(fds[i]);
    }
    tarn_rt_net_exec_drop(owner); tarn_rt_net_poll_drop(poll);
    assert(descriptors() == before);
    return 0;
}
