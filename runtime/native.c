#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
/* Tarn internal runtime ABI v0: little-endian Linux x86_64/AArch64, native C ABI. */
#define _POSIX_C_SOURCE 200809L
#include <sys/timerfd.h>
#include <stdint.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include <math.h>
#include <sys/resource.h>
#include <stdatomic.h>
/* Test tracing switches (TARN_TRACE_*), read once: getenv scans the whole
 * environment and used to run on every string drop. Tarn never changes the
 * environment, so a cached answer stays correct. */
static int tarn_trace_enabled(_Atomic int *cache, const char *name) {
    int value = atomic_load_explicit(cache, memory_order_relaxed);
    if (value < 0) {
        value = getenv(name) != NULL;
        atomic_store_explicit(cache, value, memory_order_relaxed);
    }
    return value;
}
#define TARN_TRACE(name) tarn_trace_enabled(&tarn_trace_##name, "TARN_TRACE_" #name)
static _Atomic int tarn_trace_EXEC = -1, tarn_trace_DROPS = -1, tarn_trace_SYNC = -1,
    tarn_trace_NET = -1, tarn_trace_FS = -1, tarn_trace_PROCESS = -1;

/* Opt-in allocation observations for compiler tests; no ownership decisions. */
static void exec_trace(const char *kind, const char *event, uintptr_t storage) {
    if (storage && TARN_TRACE(EXEC)) {
        flockfile(stderr);
        fprintf(stderr, "exec:%s:%s:%" PRIxPTR "\n", kind, event, storage);
        funlockfile(stderr);
    }
}

_Static_assert(sizeof(void *) == 8 && sizeof(long) == 8 && CHAR_BIT == 8,
               "Tarn requires a 64-bit Linux LP64 runtime");
typedef struct { uint64_t len; unsigned char bytes[]; } TarnString;
/* Owned string pointers are unique. Literals allocate; moves transfer ownership. */
TarnString *tarn_rt_string(const unsigned char *data, uint64_t len) {
    if (len > SIZE_MAX - sizeof(TarnString)) abort();
    TarnString *s = malloc(sizeof(TarnString) + len);
    if (!s) abort();
    s->len = len;
    if (len) memcpy(s->bytes, data, len);
    return s;
}
/* Strict UTF-8: reject overlong encodings, surrogates and values > U+10FFFF.
 * The private null result represents invalid bytes, never a Tarn string. */
TarnString *tarn_rt_string_utf8(const unsigned char *data, uint64_t len) {
    uint64_t i = 0;
    while (i < len) {
        unsigned char c = data[i++];
        if (c < 0x80) continue;
        uint64_t n;
        uint32_t cp, min;
        if (c >= 0xc2 && c <= 0xdf) { n = 1; cp = c & 0x1f; min = 0x80; }
        else if (c >= 0xe0 && c <= 0xef) { n = 2; cp = c & 0x0f; min = 0x800; }
        else if (c >= 0xf0 && c <= 0xf4) { n = 3; cp = c & 0x07; min = 0x10000; }
        else return NULL;
        if (n > len - i) return NULL;
        for (uint64_t j = 0; j < n; j++) {
            unsigned char next = data[i++];
            if ((next & 0xc0) != 0x80) return NULL;
            cp = (cp << 6) | (next & 0x3f);
        }
        if (cp < min || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) return NULL;
    }
    return tarn_rt_string(data, len);
}
TarnString *tarn_rt_string_add(const TarnString *a, const TarnString *b) {
    if (b->len > SIZE_MAX - sizeof(TarnString) - a->len) abort();
    uint64_t len = a->len + b->len;
    TarnString *s = malloc(sizeof(TarnString) + len);
    if (!s) abort();
    s->len = len;
    if (a->len) memcpy(s->bytes, a->bytes, a->len);
    if (b->len) memcpy(s->bytes + a->len, b->bytes, b->len);
    return s;
}
int32_t tarn_rt_string_compare(const TarnString *a, const TarnString *b) {
    uint64_t len = a->len < b->len ? a->len : b->len;
    int order = len ? memcmp(a->bytes, b->bytes, len) : 0;
    if (order) return order < 0 ? -1 : 1;
    return (a->len > b->len) - (a->len < b->len);
}
/* Static string literals (Phase 27): read-only TarnString images that the
 * linker gathers into the `tarn_strings` section and brackets with these
 * symbols (weak: absent when a program has no literal). */
extern const unsigned char __start_tarn_strings[] __attribute__((weak));
extern const unsigned char __stop_tarn_strings[] __attribute__((weak));
static int tarn_static_string(const TarnString *s) {
    const unsigned char *p = (const unsigned char *)s;
    return __start_tarn_strings && p >= __start_tarn_strings && p < __stop_tarn_strings;
}
void tarn_rt_drop_string(TarnString *s) {
    /* Opt-in compiler test observation, never a user destructor hook. A
     * static literal is still destroyed as a value (and traced), only its
     * storage is not released. */
    if (TARN_TRACE(DROPS)) { flockfile(stderr); fputs("drop:", stderr); fwrite(s->bytes, 1, s->len, stderr); fputc('\n', stderr); funlockfile(stderr); }
    if (tarn_static_string(s)) return;
    free(s);
}
void tarn_rt_print_i64(int64_t v) { printf("%" PRId64 "\n", v); }
void tarn_rt_print_u64(uint64_t v) { printf("%" PRIu64 "\n", v); }
void tarn_rt_print_f64(double v) { printf("%.17g\n", v); }
void tarn_rt_print_bool(uint8_t v) { puts(v ? "true" : "false"); }
void tarn_rt_print_string(const TarnString *s) { fwrite(s->bytes, 1, s->len, stdout); fputc('\n', stdout); }
_Noreturn void tarn_rt_panic(const TarnString *s) {
    fputs("panic: ", stderr); fwrite(s->bytes, 1, s->len, stderr); fputc('\n', stderr); fflush(NULL); abort();
}
/* `panic` with its call site: "panic: <message>" then "    at <path:line:col>". */
_Noreturn void tarn_rt_panic_at(const TarnString *s, const TarnString *at) {
    fputs("panic: ", stderr); fwrite(s->bytes, 1, s->len, stderr);
    fputs("\n    at ", stderr); fwrite(at->bytes, 1, at->len, stderr); fputc('\n', stderr); fflush(NULL); abort();
}
/* `tarn test --bench` (Phase 30): the runner asks for this process's CPU time
 * and peak memory, all threads included, reported once at normal exit. */
static void tarn_bench_report(void) {
    struct rusage usage;
    if (getrusage(RUSAGE_SELF, &usage)) return;
    fflush(stdout);
    fprintf(stderr, "tarn-bench-usage:%lld:%lld:%ld\n",
        (long long)usage.ru_utime.tv_sec * 1000000LL + usage.ru_utime.tv_usec,
        (long long)usage.ru_stime.tv_sec * 1000000LL + usage.ru_stime.tv_usec, usage.ru_maxrss);
}
__attribute__((constructor)) static void tarn_bench_init(void) {
    if (getenv("TARN_BENCH_RUSAGE")) atexit(tarn_bench_report);
}
_Noreturn void tarn_rt_fault(void) { fputs("panic: checked arithmetic or bounds failure\n", stderr); fflush(NULL); abort(); }
/* string.copy_range (Phase 28): a valid string cut at scalar boundaries is
 * valid UTF-8, so only the two boundary bytes are checked. */
TarnString *tarn_rt_string_range(const TarnString *s, uint64_t start, uint64_t end) {
    if (start > end || end > s->len) tarn_rt_fault();
    if (start < s->len && (s->bytes[start] & 0xc0) == 0x80) tarn_rt_fault();
    if (end < s->len && (s->bytes[end] & 0xc0) == 0x80) tarn_rt_fault();
    return tarn_rt_string(s->bytes + start, end - start);
}

float tarn_rt_rem_f32(float a, float b) { return fmodf(a, b); }
double tarn_rt_rem_f64(double a, double b) { return fmod(a, b); }

/* A closure environment header holds an internal, compiler-generated thunk.
 * Borrowed stack environments use a null thunk. Destruction and consumption
 * bodies are post-drop functions; their thunk releases owned storage. */
void *tarn_rt_env_alloc(uint64_t size) {
    void *p = malloc((size_t)size);
    if (!p) abort();
    exec_trace("storage", "alloc", (uintptr_t)p);
    return p;
}
void tarn_rt_env_free(void *p) { exec_trace("storage", "free", (uintptr_t)p); free(p); }
/* Vec storage: reallocate to `count` elements of `size` bytes. Overflow and
 * exhaustion abort. Element ownership and destruction stay in compiled code. */
void *tarn_rt_vec_grow(void *data, uint64_t count, uint64_t size) {
    if (size != 0 && count > UINT64_MAX / size) abort();
    uint64_t bytes = count * size;
    exec_trace("storage", "free", (uintptr_t)data);
    void *p = realloc(data, bytes ? (size_t)bytes : 1);
    if (!p) abort();
    exec_trace("storage", "alloc", (uintptr_t)p);
    return p;
}
/* Vec<T>.extend_from_slice for Copy T (Phase 28): header words are
 * (storage, count, capacity); growth goes through tarn_rt_vec_grow so
 * allocation tracing still sees it. */
void tarn_rt_vec_extend(uint64_t *vec, const void *src, uint64_t count, uint64_t size) {
    uint64_t len = vec[1], cap = vec[2];
    if (count > UINT64_MAX - len) abort();
    if (len + count > cap) {
        uint64_t next = cap ? cap * 2 : 4;
        if (cap > UINT64_MAX / 2 || next < len + count) next = len + count;
        vec[0] = (uint64_t)(uintptr_t)tarn_rt_vec_grow((void *)(uintptr_t)vec[0], next, size);
        vec[2] = next;
    }
    if (count) memcpy((unsigned char *)(uintptr_t)vec[0] + len * size, src, count * size);
    vec[1] = len + count;
}
void tarn_rt_env_drop(void *p) {
    if (!p) return;
    void (*drop)(void *);
    memcpy(&drop, p, sizeof(drop));
    if (drop) drop(p);
}

/* Private 11A task ABI. Worker/result adapters are generated by the compiler;
 * this record has no knowledge of Tarn types or resource layouts. */
#include <pthread.h>
#include <unistd.h>
typedef void (*TarnTaskWorker)(void *, void *, void *);
typedef void (*TarnTaskResultDrop)(void *);
typedef struct TarnTask {
    TarnTaskWorker worker;
    TarnTaskResultDrop drop_result;
    void *code;
    void *environment;
    void *result;
    pthread_mutex_t lock;
    pthread_cond_t finished;
    unsigned done;
    unsigned initialized;
    unsigned joined;
    struct TarnTask *next;
} TarnTask;

static _Noreturn void tarn_task_fault(void) {
    fputs("panic: native task runtime fault\n", stderr);
    fflush(NULL);
    abort();
}

/* Thread cache (Phase 29). A spawn runs on an idle cached thread when one is
 * waiting and otherwise on a new thread, exactly as before, so there are never
 * fewer runnable threads than tasks: joining tasks cannot deadlock on a full
 * pool. Finished threads wait for the next spawn; at most one per CPU stays
 * idle, the rest exit. Ownership, join and abort semantics are unchanged. */
static pthread_mutex_t tarn_pool_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t tarn_pool_work = PTHREAD_COND_INITIALIZER;
static TarnTask *tarn_pool_head, *tarn_pool_tail;
static unsigned long tarn_pool_waiting, tarn_pool_signals;

static unsigned long tarn_pool_max_idle(void) {
    static unsigned long cached;
    if (!cached) { long n = sysconf(_SC_NPROCESSORS_ONLN); cached = n > 0 ? (unsigned long)n : 1; }
    return cached;
}

static void tarn_task_run(TarnTask *task) {
    task->worker(task->result, task->code, task->environment);
    pthread_mutex_lock(&task->lock);
    task->initialized = 1;
    task->done = 1;
    pthread_cond_signal(&task->finished);
    pthread_mutex_unlock(&task->lock);
}

static void *tarn_pool_thread(void *argument) {
    TarnTask *task = argument;
    for (;;) {
        tarn_task_run(task);
        pthread_mutex_lock(&tarn_pool_lock);
        while (!tarn_pool_head) {
            if (tarn_pool_waiting >= tarn_pool_max_idle()) { pthread_mutex_unlock(&tarn_pool_lock); return NULL; }
            ++tarn_pool_waiting;
            pthread_cond_wait(&tarn_pool_work, &tarn_pool_lock);
            --tarn_pool_waiting;
            if (tarn_pool_signals) --tarn_pool_signals;
        }
        task = tarn_pool_head;
        tarn_pool_head = task->next;
        if (!tarn_pool_head) tarn_pool_tail = NULL;
        pthread_mutex_unlock(&tarn_pool_lock);
    }
}

TarnTask *tarn_rt_task_spawn(TarnTaskWorker worker, TarnTaskResultDrop drop_result,
                           uint64_t result_size, void *code, void *environment) {
    if (!worker || !drop_result || result_size > 65536) tarn_task_fault();
    TarnTask *task = calloc(1, sizeof(*task));
    if (!task) tarn_task_fault();
    task->result = calloc(1, result_size ? (size_t)result_size : 1);
    if (!task->result) tarn_task_fault();
    task->worker = worker;
    task->drop_result = drop_result;
    task->code = code;
    task->environment = environment;
    if (pthread_mutex_init(&task->lock, NULL) || pthread_cond_init(&task->finished, NULL)) tarn_task_fault();
    pthread_mutex_lock(&tarn_pool_lock);
    if (tarn_pool_waiting > tarn_pool_signals) {
        /* An idle thread is guaranteed to wake and find this task. */
        if (tarn_pool_tail) tarn_pool_tail->next = task; else tarn_pool_head = task;
        tarn_pool_tail = task;
        ++tarn_pool_signals;
        pthread_cond_signal(&tarn_pool_work);
        pthread_mutex_unlock(&tarn_pool_lock);
        return task;
    }
    pthread_mutex_unlock(&tarn_pool_lock);
    pthread_t thread;
    pthread_attr_t attributes;
    if (pthread_attr_init(&attributes) || pthread_attr_setdetachstate(&attributes, PTHREAD_CREATE_DETACHED)
        || pthread_create(&thread, &attributes, tarn_pool_thread, task)) tarn_task_fault();
    pthread_attr_destroy(&attributes);
    return task;
}

/* Borrowed result storage, valid until release; ownership transfers only when
 * generated join code moves its initialized value into caller storage. */
void *tarn_rt_task_wait(TarnTask *task) {
    if (!task || task->joined) tarn_task_fault();
    pthread_mutex_lock(&task->lock);
    while (!task->done) pthread_cond_wait(&task->finished, &task->lock);
    pthread_mutex_unlock(&task->lock);
    task->joined = 1;
    if (!task->initialized) tarn_task_fault();
    return task->result;
}

/* Called only after generated code has moved the result out. */
void tarn_rt_task_release(TarnTask *task) {
    if (!task || !task->joined || !task->initialized) tarn_task_fault();
    task->initialized = 0;
    pthread_cond_destroy(&task->finished);
    pthread_mutex_destroy(&task->lock);
    free(task->result);
    free(task);
}

void tarn_rt_task_drop(TarnTask *task) {
    void *result = tarn_rt_task_wait(task);
    task->drop_result(result);
    tarn_rt_task_release(task);
}


/* Private 11C ABI: native mechanics only. Payloads and destruction belong to
 * verified Tarn values. No recursive locking, poisoning or ownership bit. */
#include <stdatomic.h>
#include <stdbool.h>
#include <limits.h>
typedef struct { pthread_mutex_t mutex; } TarnMutex;
static void tarn_sync_trace(const char *event) {
    if (TARN_TRACE(SYNC)) {
        flockfile(stderr); fprintf(stderr, "sync:%s\n", event); funlockfile(stderr);
    }
}
TarnMutex *tarn_rt_mutex_create(void) {
    TarnMutex *m = malloc(sizeof(*m));
    if (!m || pthread_mutex_init(&m->mutex, NULL)) abort();
    tarn_sync_trace("create");
    return m;
}
void tarn_rt_mutex_lock(TarnMutex *m) {
    if (pthread_mutex_lock(&m->mutex)) abort();
    tarn_sync_trace("lock");
}
void tarn_rt_mutex_unlock(TarnMutex *m) {
    tarn_sync_trace("unlock");
    if (pthread_mutex_unlock(&m->mutex)) abort();
}
void tarn_rt_mutex_destroy(TarnMutex *m) {
    if (pthread_mutex_destroy(&m->mutex)) abort();
    tarn_sync_trace("destroy");
    free(m);
}

/* Each scalar is real C11 atomic storage. All operations, including failure
 * of compare/exchange, are sequentially consistent. Never expose its address. */
#define TARN_ATOMIC(NAME, TYPE) \
typedef struct { _Atomic(TYPE) value; } TarnAtomic##NAME; \
TarnAtomic##NAME *tarn_rt_atomic_##NAME##_new(TYPE value) { \
    TarnAtomic##NAME *a = malloc(sizeof(*a)); if (!a) abort(); \
    atomic_init(&a->value, value); return a; \
} \
TYPE tarn_rt_atomic_##NAME##_load(TarnAtomic##NAME *a) { \
    return atomic_load_explicit(&a->value, memory_order_seq_cst); \
} \
void tarn_rt_atomic_##NAME##_store(TarnAtomic##NAME *a, TYPE value) { \
    atomic_store_explicit(&a->value, value, memory_order_seq_cst); \
} \
TYPE tarn_rt_atomic_##NAME##_swap(TarnAtomic##NAME *a, TYPE value) { \
    return atomic_exchange_explicit(&a->value, value, memory_order_seq_cst); \
} \
uint8_t tarn_rt_atomic_##NAME##_compare_exchange(TarnAtomic##NAME *a, TYPE expected, TYPE value) { \
    return atomic_compare_exchange_strong_explicit(&a->value, &expected, value, memory_order_seq_cst, memory_order_seq_cst); \
}
TARN_ATOMIC(bool, uint8_t)
TARN_ATOMIC(i32, int32_t)
TARN_ATOMIC(i64, int64_t)
TARN_ATOMIC(u32, uint32_t)
TARN_ATOMIC(u64, uint64_t)
TARN_ATOMIC(usize, uintptr_t)

/* Checked CAS loops: failed exchanges retry from the new observed value.
 * Overflow aborts without writing; no signed C overflow is ever evaluated. */
#define TARN_ATOMIC_SIGNED(NAME, TYPE, MINIMUM, MAXIMUM) \
TYPE tarn_rt_atomic_##NAME##_fetch_add(TarnAtomic##NAME *a, TYPE value) { \
    TYPE old = atomic_load_explicit(&a->value, memory_order_seq_cst); \
    for (;;) { \
        if ((value > 0 && old > MAXIMUM - value) || (value < 0 && old < MINIMUM - value)) tarn_rt_fault(); \
        TYPE next = old + value; \
        if (atomic_compare_exchange_strong_explicit(&a->value, &old, next, memory_order_seq_cst, memory_order_seq_cst)) return old; \
    } \
} \
TYPE tarn_rt_atomic_##NAME##_fetch_sub(TarnAtomic##NAME *a, TYPE value) { \
    TYPE old = atomic_load_explicit(&a->value, memory_order_seq_cst); \
    for (;;) { \
        if ((value > 0 && old < MINIMUM + value) || (value < 0 && old > MAXIMUM + value)) tarn_rt_fault(); \
        TYPE next = old - value; \
        if (atomic_compare_exchange_strong_explicit(&a->value, &old, next, memory_order_seq_cst, memory_order_seq_cst)) return old; \
    } \
}
#define TARN_ATOMIC_UNSIGNED(NAME, TYPE, MAXIMUM) \
TYPE tarn_rt_atomic_##NAME##_fetch_add(TarnAtomic##NAME *a, TYPE value) { \
    TYPE old = atomic_load_explicit(&a->value, memory_order_seq_cst); \
    for (;;) { \
        if (old > MAXIMUM - value) tarn_rt_fault(); \
        TYPE next = old + value; \
        if (atomic_compare_exchange_strong_explicit(&a->value, &old, next, memory_order_seq_cst, memory_order_seq_cst)) return old; \
    } \
} \
TYPE tarn_rt_atomic_##NAME##_fetch_sub(TarnAtomic##NAME *a, TYPE value) { \
    TYPE old = atomic_load_explicit(&a->value, memory_order_seq_cst); \
    for (;;) { \
        if (old < value) tarn_rt_fault(); \
        TYPE next = old - value; \
        if (atomic_compare_exchange_strong_explicit(&a->value, &old, next, memory_order_seq_cst, memory_order_seq_cst)) return old; \
    } \
}
TARN_ATOMIC_SIGNED(i32, int32_t, INT32_MIN, INT32_MAX)
TARN_ATOMIC_SIGNED(i64, int64_t, INT64_MIN, INT64_MAX)
TARN_ATOMIC_UNSIGNED(u32, uint32_t, UINT32_MAX)
TARN_ATOMIC_UNSIGNED(u64, uint64_t, UINT64_MAX)
TARN_ATOMIC_UNSIGNED(usize, uintptr_t, UINTPTR_MAX)
void tarn_rt_atomic_destroy(void *a) { free(a); }

/* Private blocking networking bridge. No ownership inference or public errno API. */
#include <errno.h>
#include <stddef.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <netdb.h>
#include <arpa/inet.h>
#include <unistd.h>
typedef struct { uint8_t bytes[24]; } TarnNetAddr;
typedef struct { int32_t domain, code; int64_t value; TarnNetAddr address; } TarnNetRaw;
_Static_assert(sizeof(TarnNetRaw) == 40 && offsetof(TarnNetRaw, address) == 16, "network ABI");
static void net_init(TarnNetRaw *out) { memset(out, 0, sizeof(*out)); }
static void net_status(TarnNetRaw *out, int64_t value) { out->value = value; if (value < 0) out->code = errno; }
static void net_trace(const char *event, int fd) {
    if (TARN_TRACE(NET)) { flockfile(stderr); fprintf(stderr, "net:%s:%d\n", event, fd); funlockfile(stderr); }
}
static int net_native(const TarnNetAddr *address, struct sockaddr_storage *storage, socklen_t *len) {
    memset(storage, 0, sizeof(*storage));
    if (address->bytes[18] == 4) {
        struct sockaddr_in *a = (struct sockaddr_in *)storage;
        a->sin_family = AF_INET; memcpy(&a->sin_addr, address->bytes, 4); memcpy(&a->sin_port, address->bytes + 16, 2);
        *len = sizeof(*a); return AF_INET;
    }
    if (address->bytes[18] == 6) {
        struct sockaddr_in6 *a = (struct sockaddr_in6 *)storage;
        a->sin6_family = AF_INET6; memcpy(&a->sin6_addr, address->bytes, 16); memcpy(&a->sin6_port, address->bytes + 16, 2);
        uint32_t scope; memcpy(&scope, address->bytes + 20, 4); a->sin6_scope_id = ntohl(scope);
        *len = sizeof(*a); return AF_INET6;
    }
    errno = EINVAL; return -1;
}
/* Only IPv4/IPv6 fit SocketAddr. Any other family (e.g. an AF_UNIX descriptor
 * reaching an address query) is an error result, never an abort (ADR 0069). */
static int net_address(TarnNetAddr *address, const struct sockaddr *sa) {
    memset(address, 0, sizeof(*address));
    if (sa->sa_family == AF_INET) {
        const struct sockaddr_in *a = (const struct sockaddr_in *)sa;
        memcpy(address->bytes, &a->sin_addr, 4); memcpy(address->bytes + 16, &a->sin_port, 2); address->bytes[18] = 4;
    } else if (sa->sa_family == AF_INET6) {
        const struct sockaddr_in6 *a = (const struct sockaddr_in6 *)sa;
        memcpy(address->bytes, &a->sin6_addr, 16); memcpy(address->bytes + 16, &a->sin6_port, 2); address->bytes[18] = 6;
        uint32_t scope = htonl(a->sin6_scope_id); memcpy(address->bytes + 20, &scope, 4);
    } else return -1;
    return 0;
}
void tarn_rt_net_resolve(TarnNetRaw *out, const TarnString *endpoint) {
    net_init(out);
    if (!endpoint->len || endpoint->len > 4096 || memchr(endpoint->bytes, 0, endpoint->len)) { out->domain = 2; out->code = EINVAL; return; }
    char *text = malloc(endpoint->len + 1); if (!text) abort();
    memcpy(text, endpoint->bytes, endpoint->len); text[endpoint->len] = 0;
    char *host = text, *port = NULL;
    if (*text == '[') { char *end = strchr(text, ']'); if (end && end[1] == ':') { *end = 0; host++; port = end + 2; } }
    else { char *colon = strrchr(text, ':'); if (colon && !memchr(text, ':', (size_t)(colon - text))) { *colon = 0; port = colon + 1; } }
    unsigned number = 0; int valid = port && *port;
    if (valid) for (const char *c = port; *c; c++) { if (*c < '0' || *c > '9' || number > 65535 / 10) { valid = 0; break; } number = number * 10 + (unsigned)(*c - '0'); if (number > 65535) { valid = 0; break; } }
    if (!valid) { free(text); out->domain = 2; out->code = EINVAL; return; }
    struct addrinfo hints = {0}, *list = NULL;
    hints.ai_family = AF_UNSPEC; hints.ai_socktype = SOCK_STREAM; hints.ai_flags = AI_NUMERICSERV | (!*host ? AI_PASSIVE : 0);
    int error = getaddrinfo(*host ? host : NULL, port, &hints, &list);
    if (error) { out->domain = error == EAI_SYSTEM ? 0 : 1; out->code = error == EAI_SYSTEM ? errno : error; free(text); return; }
    struct addrinfo *choice = NULL;
    for (struct addrinfo *a = list; a; a = a->ai_next) { if (a->ai_family == AF_INET) { choice = a; break; } if (!choice && a->ai_family == AF_INET6) choice = a; }
    if (choice) net_address(&out->address, choice->ai_addr); else { out->domain = 1; out->code = EAI_FAMILY; }
    freeaddrinfo(list); free(text);
}
void tarn_rt_net_socket(TarnNetRaw *out, const TarnNetAddr *address, uint8_t udp) {
    net_init(out); struct sockaddr_storage sa; socklen_t len;
    int family = net_native(address, &sa, &len);
    if (family < 0) { out->code = errno; return; }
    net_status(out, socket(family, udp ? SOCK_DGRAM : SOCK_STREAM, 0));
    if (!out->code) net_trace("open", (int)out->value);
}
void tarn_rt_net_bind(TarnNetRaw *out, int32_t fd, const TarnNetAddr *address) {
    net_init(out); struct sockaddr_storage sa; socklen_t len;
    if (net_native(address, &sa, &len) < 0) { out->code = errno; return; }
    /* TCP listeners reuse addresses held in TIME_WAIT by connections the
       server closed; Linux still rejects a second active listener. */
    int type = 0, one = 1; socklen_t size = sizeof(type);
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &size) == 0 && type == SOCK_STREAM)
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    net_status(out, bind(fd, (struct sockaddr *)&sa, len));
}
void tarn_rt_net_listen(TarnNetRaw *out, int32_t fd) { net_init(out); net_status(out, listen(fd, SOMAXCONN)); }
void tarn_rt_net_accept(TarnNetRaw *out, int32_t fd) { net_init(out); net_status(out, accept(fd, NULL, NULL)); if (!out->code) net_trace("open", (int)out->value); }
void tarn_rt_net_connect(TarnNetRaw *out, int32_t fd, const TarnNetAddr *address) {
    net_init(out); struct sockaddr_storage sa; socklen_t len;
    if (net_native(address, &sa, &len) < 0) { out->code = errno; return; }
    net_status(out, connect(fd, (struct sockaddr *)&sa, len));
}
/* Unix-domain stream client (ADR 0069). The path is bounded, NUL-free
 * filesystem text; abstract names are not supported. A failed connect closes
 * the fresh descriptor here, before any Tarn owner exists; EINTR is returned
 * so the Tarn caller retries with a new socket, never this connect. */
void tarn_rt_net_connect_unix(TarnNetRaw *out, const TarnString *path) {
    net_init(out); struct sockaddr_un sa; memset(&sa, 0, sizeof(sa)); sa.sun_family = AF_UNIX;
    if (!path->len || path->len >= sizeof(sa.sun_path) || memchr(path->bytes, 0, path->len)) { out->domain = 2; out->code = EINVAL; return; }
    memcpy(sa.sun_path, path->bytes, path->len);
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) { out->code = errno; return; }
    if (connect(fd, (struct sockaddr *)&sa, (socklen_t)(offsetof(struct sockaddr_un, sun_path) + path->len + 1)) < 0) {
        out->code = errno; close(fd); return;
    }
    out->value = fd; net_trace("open", fd);
}
/* Connected Unix stream pair (ADR 0070): value is the first descriptor and
 * address bytes 0..4 the second, little-endian. Both are close-on-exec; only
 * an explicit process handoff makes one visible to a child. */
void tarn_rt_net_pair_unix(TarnNetRaw *out) {
    net_init(out); int fds[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds) < 0) { out->code = errno; return; }
    out->value = fds[0];
    for (int i = 0; i < 4; ++i) out->address.bytes[i] = (uint8_t)((uint32_t)fds[1] >> (i * 8));
    net_trace("open", fds[0]); net_trace("open", fds[1]);
}
void tarn_rt_net_read(TarnNetRaw *out, int32_t fd, uint8_t *bytes, uint64_t len) {
    net_init(out); if (len) net_status(out, recv(fd, bytes, len, 0));
}
void tarn_rt_net_write(TarnNetRaw *out, int32_t fd, const uint8_t *bytes, uint64_t len) { net_init(out); net_status(out, send(fd, bytes, len, MSG_NOSIGNAL)); }
void tarn_rt_net_recv(TarnNetRaw *out, int32_t fd, uint8_t *bytes, uint64_t len) {
    net_init(out); struct sockaddr_storage sa; socklen_t size = sizeof(sa);
    net_status(out, recvfrom(fd, bytes, len, 0, (struct sockaddr *)&sa, &size));
    if (!out->code && net_address(&out->address, (struct sockaddr *)&sa) < 0) out->code = EAFNOSUPPORT;
}
void tarn_rt_net_send(TarnNetRaw *out, int32_t fd, const uint8_t *bytes, uint64_t len, const TarnNetAddr *address) {
    net_init(out); struct sockaddr_storage sa; socklen_t size;
    if (net_native(address, &sa, &size) < 0) { out->code = errno; return; }
    net_status(out, sendto(fd, bytes, len, MSG_NOSIGNAL, (struct sockaddr *)&sa, size));
}
void tarn_rt_net_addr(TarnNetRaw *out, int32_t fd, uint8_t peer) {
    net_init(out); struct sockaddr_storage sa; socklen_t size = sizeof(sa);
    net_status(out, peer ? getpeername(fd, (struct sockaddr *)&sa, &size) : getsockname(fd, (struct sockaddr *)&sa, &size));
    if (!out->code && net_address(&out->address, (struct sockaddr *)&sa) < 0) out->code = EAFNOSUPPORT;
}
void tarn_rt_net_shutdown(TarnNetRaw *out, int32_t fd, int32_t how) { net_init(out); net_status(out, shutdown(fd, how)); }
static void net_timer_track(int32_t fd);
static void net_timer_forget(int32_t fd);
void tarn_rt_net_close(TarnNetRaw *out, int32_t fd) {
    /* Observe the release attempt before close makes fd reusable by another
     * task. Logging after close can misorder reuse and falsely report two owners.
     * This is only test observation; the syscall remains the actual release. */
    net_trace("close", fd);
    net_timer_forget(fd); /* before close: the number may be reused at once */
    net_init(out); net_status(out, close(fd));
    if (out->code == EBADF) abort(); /* impossible with a verified live owner */
    /* Linux consumes fd even when close reports EINTR: never retry. */
}
void tarn_rt_net_drop(int32_t fd) { TarnNetRaw out; tarn_rt_net_close(&out, fd); }
/* Phase 14B one-shot timer: a nonblocking CLOCK_MONOTONIC timerfd, readable once
 * it expires, so it shares the 12B Poll. No thread, no sleep. Zero fires at once
 * (a zero it_value would disarm instead). Closed like sockets by Timer's owner. */
void tarn_rt_net_timer_new(TarnNetRaw *out, int64_t millis) {
    net_init(out);
    if (millis < 0) { out->code = EINVAL; return; }
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    if (fd < 0) { out->code = errno; return; }
    struct itimerspec spec; memset(&spec, 0, sizeof(spec));
    spec.it_value.tv_sec = (time_t)(millis / 1000);
    spec.it_value.tv_nsec = (long)(millis % 1000) * 1000000L;
    if (millis == 0) spec.it_value.tv_nsec = 1;
    if (timerfd_settime(fd, 0, &spec, NULL) < 0) { out->code = errno; close(fd); return; }
    net_timer_track(fd);
    out->value = fd; net_trace("open", fd);
}
void tarn_rt_net_timer_read(TarnNetRaw *out, int32_t fd) {
    uint64_t expirations; net_init(out); net_status(out, read(fd, &expirations, sizeof(expirations)));
}

void tarn_rt_net_main_error(uint32_t kind, int32_t code) {
    static const char *names[] = {"address in use", "connection refused", "connection reset", "broken pipe", "timed out", "would block", "invalid address", "DNS failure", "other OS error", "write made no progress", "unexpected EOF", "limit exceeded", "not found", "permission denied", "already exists", "invalid input", "invalid data", "not a directory", "is a directory", "directory not empty", "storage full"};
    if (kind >= sizeof(names) / sizeof(names[0])) abort();
    fprintf(stderr, "I/O error: %s (native code %" PRId32 ")\n", names[kind], code);
}

/* Phase 12B: no application pointers survive any of these calls. */
#include <sys/epoll.h>
#include <fcntl.h>
#include <time.h>
#ifndef SO_COOKIE
#define SO_COOKIE 57
#endif
/* Poll bookkeeping does not own sockets. Cookie checks prevent fd reuse from
   turning an old token into modification/deletion authority over a new socket. */
typedef struct TarnRegistration {
    int fd;
    uint64_t cookie, token;
    struct TarnRegistration *next;
} TarnRegistration;
typedef struct { int fd; TarnRegistration *head; } TarnPoll;
typedef struct { uint64_t token; uint8_t readable, writable, error, hangup; uint8_t padding[4]; } TarnEvent;
_Static_assert(sizeof(TarnEvent) == 16 && offsetof(TarnEvent, hangup) == 11, "event ABI");
static _Atomic uint64_t net_next_token = 1;
void tarn_rt_net_nonblocking(TarnNetRaw *out, int32_t fd, uint8_t enabled) {
    net_init(out);
    int mode = fcntl(fd, F_GETFL);
    if (mode < 0) { out->code = errno; return; }
    net_status(out, fcntl(fd, F_SETFL, enabled ? mode | O_NONBLOCK : mode & ~O_NONBLOCK));
}
void tarn_rt_net_mode(TarnNetRaw *out, int32_t fd) {
    net_init(out); int mode = fcntl(fd, F_GETFL);
    if (mode < 0) out->code = errno; else out->value = !!(mode & O_NONBLOCK);
}
void tarn_rt_net_connected(TarnNetRaw *out, int32_t fd) {
    net_init(out); int error = 0; socklen_t n = sizeof(error);
    if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &n) < 0) { out->code = errno; return; }
    if (error) { out->code = error; return; }
    struct sockaddr_storage peer; n = sizeof(peer);
    if (getpeername(fd, (struct sockaddr *)&peer, &n) < 0) out->code = errno == ENOTCONN ? EAGAIN : errno;
}
void tarn_rt_net_now(TarnNetRaw *out) {
    net_init(out); struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) < 0) out->code = errno;
    else out->value = (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}
void tarn_rt_net_poll_new(TarnNetRaw *out) {
    net_init(out); int fd = epoll_create1(EPOLL_CLOEXEC);
    if (fd < 0) { out->code = errno; return; }
    TarnPoll *poll = malloc(sizeof(*poll));
    if (!poll) { int error = ENOMEM; close(fd); out->code = error; return; }
    poll->fd = fd; poll->head = NULL; out->value = (int64_t)(uintptr_t)poll;
    net_trace("open", fd);
}
/* Timers have no SO_COOKIE and share one anonymous inode, so each live timer
 * gets a process-unique identity (high bit set: never a socket cookie). */
typedef struct TarnTimerId { int32_t fd; uint64_t id; struct TarnTimerId *next; } TarnTimerId;
static TarnTimerId *net_timers;
static uint64_t net_timer_next = 1;
static pthread_mutex_t net_timer_lock = PTHREAD_MUTEX_INITIALIZER;
static void net_timer_track(int32_t fd) {
    TarnTimerId *entry = malloc(sizeof(*entry)); if (!entry) abort();
    pthread_mutex_lock(&net_timer_lock);
    entry->fd = fd; entry->id = (net_timer_next++) | (UINT64_C(1) << 63); entry->next = net_timers; net_timers = entry;
    pthread_mutex_unlock(&net_timer_lock);
}
static void net_timer_forget(int32_t fd) {
    pthread_mutex_lock(&net_timer_lock);
    for (TarnTimerId **link = &net_timers; *link; link = &(*link)->next)
        if ((*link)->fd == fd) { TarnTimerId *old = *link; *link = old->next; free(old); break; }
    pthread_mutex_unlock(&net_timer_lock);
}
static int net_identity_of(int32_t fd, uint64_t *cookie) {
    socklen_t n = sizeof(*cookie);
    if (getsockopt(fd, SOL_SOCKET, SO_COOKIE, cookie, &n) == 0) return 0;
    if (errno != ENOTSOCK) return -1;
    int found = 0;
    pthread_mutex_lock(&net_timer_lock);
    for (TarnTimerId *t = net_timers; t; t = t->next) if (t->fd == fd) { *cookie = t->id; found = 1; break; }
    pthread_mutex_unlock(&net_timer_lock);
    if (!found) errno = ENOTSOCK;
    return found ? 0 : -1;
}
void tarn_rt_net_poll_ctl(TarnNetRaw *out, TarnPoll *poll, int32_t fd, int32_t operation, int32_t interest, uint64_t token) {
    net_init(out); uint64_t cookie;
    if (operation < 0 || operation > 2 || interest < 1 || interest > 3) { out->code = EINVAL; return; }
    if (net_identity_of(fd, &cookie) < 0) { out->code = errno; return; }
    TarnRegistration **link = &poll->head;
    while (*link && (*link)->fd != fd) link = &(*link)->next;
    TarnRegistration *record = *link;
    if (operation && (!record || record->token != token || record->cookie != cookie)) { out->code = ENOENT; return; }
    if (!operation && record && record->cookie == cookie) { out->code = EEXIST; return; }
    TarnRegistration *fresh = NULL;
    if (!operation) {
        fresh = malloc(sizeof(*fresh));
        if (!fresh) { out->code = ENOMEM; return; }
        /* Never wrap or reuse. Signed raw value preserves checked u64 conversion. */
        token = atomic_load(&net_next_token);
        for (;;) {
            if (token >= INT64_MAX) { free(fresh); out->code = EOVERFLOW; return; }
            if (atomic_compare_exchange_weak(&net_next_token, &token, token + 1)) break;
        }
    }
    struct epoll_event event = {0};
    event.events = EPOLLRDHUP | ((interest & 1) ? EPOLLIN : 0) | ((interest & 2) ? EPOLLOUT : 0);
    event.data.u64 = token;
    int op = operation == 0 ? EPOLL_CTL_ADD : operation == 1 ? EPOLL_CTL_MOD : EPOLL_CTL_DEL;
    if (epoll_ctl(poll->fd, op, fd, &event) < 0) { out->code = errno; free(fresh); return; }
    if (!operation) {
        fresh->fd = fd; fresh->cookie = cookie; fresh->token = token;
        fresh->next = record ? record->next : NULL; *link = fresh; free(record);
    } else if (operation == 2) { *link = record->next; free(record); }
    out->value = (int64_t)token;
}
void tarn_rt_net_poll_wait(TarnNetRaw *out, TarnPoll *poll, TarnEvent *events, uint64_t length, int32_t timeout) {
    net_init(out);
    if (!length || timeout < -1) { out->code = EINVAL; return; }
    struct epoll_event native[64];
    int count = epoll_wait(poll->fd, native, length < 64 ? (int)length : 64, timeout);
    if (count < 0) { out->code = errno; return; }
    for (int i = 0; i < count; ++i) {
        memset(&events[i], 0, sizeof(events[i]));
        events[i].token = native[i].data.u64;
        events[i].readable = !!(native[i].events & EPOLLIN);
        events[i].writable = !!(native[i].events & EPOLLOUT);
        events[i].error = !!(native[i].events & EPOLLERR);
        events[i].hangup = !!(native[i].events & (EPOLLHUP | EPOLLRDHUP));
    }
    out->value = count;
}
void tarn_rt_net_close_poll(TarnNetRaw *out, TarnPoll *poll) {
    tarn_rt_net_close(out, poll->fd);
    while (poll->head) { TarnRegistration *record = poll->head; poll->head = record->next; free(record); }
    free(poll);
}
void tarn_rt_net_poll_drop(TarnPoll *poll) { TarnNetRaw out; tarn_rt_net_close_poll(&out, poll); }

/* Mechanical wake bookkeeping, not an executor or application-state store. */
typedef struct TarnWake TarnWake;
typedef struct TarnSpawned TarnSpawned;
typedef struct { TarnPoll *poll; TarnWake *head; TarnSpawned *inbox, *inbox_tail; } TarnExecution;
struct TarnWake {
    TarnExecution *owner;
    TarnWake *next;
    uint64_t identity, token, parent;
    int fd;
    uint8_t queued;
};
static uint64_t net_identity(void) {
    uint64_t id = atomic_load(&net_next_token);
    for (;;) {
        if (id >= INT64_MAX) tarn_rt_fault();
        if (atomic_compare_exchange_weak(&net_next_token, &id, id + 1)) return id;
    }
}
void tarn_rt_net_exec_new(TarnNetRaw *out, TarnPoll *poll) {
    net_init(out); TarnExecution *owner = malloc(sizeof(*owner));
    if (!owner) { out->code = ENOMEM; return; }
    owner->poll = poll; owner->head = NULL; owner->inbox = owner->inbox_tail = NULL; out->value = (int64_t)(uintptr_t)owner;
}
void tarn_rt_net_waker_new(uintptr_t *out, TarnExecution *owner) {
    TarnWake *wake = calloc(1, sizeof(*wake)); if (!wake) tarn_rt_fault();
    exec_trace("waker", "alloc", (uintptr_t)wake);
    wake->identity = net_identity(); wake->owner = owner; wake->fd = -1;
    wake->queued = 1; wake->next = owner->head; owner->head = wake;
    *out = (uintptr_t)wake;
}
static TarnWake *net_wake_find(TarnExecution *owner, uint64_t id) {
    /* Zero is the absent-parent sentinel; identities start at one. */
    if (!id) return NULL;
    for (TarnWake *wake = owner->head; wake; wake = wake->next) if (wake->identity == id) return wake;
    return NULL;
}
static void net_wake_deliver(TarnWake *wake) {
    for (int depth = 0; wake; ++depth) {
        if (depth >= 64) tarn_rt_fault();
        wake->queued = 1;
        wake = net_wake_find(wake->owner, wake->parent);
    }
}
void tarn_rt_net_wake(TarnNetRaw *out, TarnWake *wake) { net_init(out); net_wake_deliver(wake); }
void tarn_rt_net_wake_link(TarnNetRaw *out, TarnWake *child, TarnWake *parent) {
    net_init(out);
    if (child->owner != parent->owner) { out->code = EINVAL; return; }
    TarnWake *next = parent;
    for (int depth = 0; next; ++depth) {
        if (next == child || depth >= 63) { out->code = EINVAL; return; }
        next = net_wake_find(next->owner, next->parent);
    }
    child->parent = parent->identity;
}
void tarn_rt_net_wake_owner(TarnNetRaw *out, TarnWake *wake, TarnExecution *owner) {
    net_init(out); if (wake->owner != owner) out->code = EINVAL;
}
void tarn_rt_net_wake_take(TarnNetRaw *out, TarnWake *wake) { net_init(out); out->value = wake->queued; wake->queued = 0; }
void tarn_rt_net_wake_arm(TarnNetRaw *out, TarnWake *wake, int32_t fd, int32_t interest) {
    if (wake->token && wake->fd != fd) { net_init(out); out->code = EINVAL; return; }
    tarn_rt_net_poll_ctl(out, wake->owner->poll, fd, wake->token ? 1 : 0, interest, wake->token);
    if (!out->code) { wake->fd = fd; wake->token = out->value; }
}
void tarn_rt_net_wake_clear(TarnNetRaw *out, TarnWake *wake) {
    net_init(out); wake->queued = 0;
    if (!wake->token) return;
    TarnPoll *poll = wake->owner->poll;
    /* Reuse the 12B token/cookie check before touching any live fd. */
    TarnNetRaw status; tarn_rt_net_poll_ctl(&status, poll, wake->fd, 2, 1, wake->token);
    /* A captured owner may already have closed. Retire only our token record;
       never erase replacement bookkeeping or modify an unrelated recycled fd. */
    TarnRegistration **link = &poll->head;
    while (*link && (*link)->token != wake->token) link = &(*link)->next;
    if (*link) { TarnRegistration *old = *link; *link = old->next; free(old); }
    wake->token = 0; wake->fd = -1;
}
void tarn_rt_net_exec_wait(TarnNetRaw *out, TarnExecution *owner, int32_t timeout) {
    TarnEvent events[64]; tarn_rt_net_poll_wait(out, owner->poll, events, 64, timeout);
    if (out->code) return;
    for (int i = 0; i < out->value; ++i) {
        for (TarnWake *wake = owner->head; wake; wake = wake->next)
            if (wake->token && wake->token == events[i].token) net_wake_deliver(wake);
    }
}
void tarn_rt_net_waker_drop(TarnWake *wake) {
    TarnNetRaw out; tarn_rt_net_wake_clear(&out, wake);
    TarnWake **link = &wake->owner->head;
    while (*link && *link != wake) link = &(*link)->next;
    if (!*link) tarn_rt_fault();
    *link = wake->next; exec_trace("waker", "free", (uintptr_t)wake); free(wake);
}
void tarn_rt_net_exec_drop(TarnExecution *owner) {
    if (owner->head) tarn_rt_fault(); /* Ordinary loans must keep Execution alive. */
    if (owner->inbox) tarn_rt_fault(); /* Spawned operations are drained by Tarn executors. */
    free(owner);
}

/* Phase 14A async task records. Storage only: compiled code copies and destroys
 * the typed result; Tarn decides scheduling, completion and abandonment. Two
 * references exist: the AsyncTask handle and the executor's runner entry. */
typedef struct {
    TarnExecution *owner;
    uint64_t joiner;     /* identity of a waiting Waker, 0 if none */
    uint32_t refs, complete, taken, abandoned;
    unsigned char result[];
} TarnAsyncTask;
TarnAsyncTask *tarn_rt_async_task_new(TarnExecution *owner, uint64_t size) {
    TarnAsyncTask *task = calloc(1, sizeof(TarnAsyncTask) + (size ? size : 1));
    if (!task) abort();
    task->owner = owner; task->refs = 2;
    exec_trace("task", "alloc", (uintptr_t)task);
    return task;
}
void *tarn_rt_async_task_result(TarnAsyncTask *task) { return task->result; }
/* After compiled code stored the result: mark complete and wake the joiner. */
void tarn_rt_async_task_complete(TarnAsyncTask *task) {
    if (task->complete || task->abandoned) tarn_rt_fault();
    task->complete = 1;
    if (task->joiner) { TarnWake *w = net_wake_find(task->owner, task->joiner); if (w) net_wake_deliver(w); }
}
/* 1 when an untaken result may be moved out now (marks it taken). */
int8_t tarn_rt_async_task_take(TarnAsyncTask *task) {
    if (!task->complete || task->taken) return 0;
    task->taken = 1; return 1;
}
/* Register the joining Waker; it must belong to the task's Execution. */
void tarn_rt_async_task_wait(TarnNetRaw *out, TarnAsyncTask *task, TarnWake *waker) {
    net_init(out);
    if (waker->owner != task->owner) { out->code = EINVAL; return; }
    task->joiner = waker->identity;
}
int8_t tarn_rt_async_task_abandoned(TarnAsyncTask *task) { return (int8_t)task->abandoned; }
void tarn_rt_async_task_release(TarnAsyncTask *task) {
    if (task->refs == 0) tarn_rt_fault();
    if (--task->refs == 0) { exec_trace("task", "free", (uintptr_t)task); free(task); }
}
/* Handle destruction: 1 when compiled code must destroy an untaken result.
 * Otherwise a pending task is marked abandoned for structured destruction. */
int8_t tarn_rt_async_task_handle_drop(TarnAsyncTask *task) {
    task->joiner = 0;
    if (task->complete && !task->taken) { task->taken = 1; return 1; }
    if (!task->complete) task->abandoned = 1;
    return 0;
}

/* Spawn mailbox: owned operation bytes moved in by Tarn, drained FIFO. */
struct TarnSpawned { TarnSpawned *next; uint64_t size; unsigned char bytes[]; };
void tarn_rt_async_inbox_push(TarnExecution *owner, const void *bytes, uint64_t size) {
    TarnSpawned *entry = malloc(sizeof(TarnSpawned) + (size ? size : 1));
    if (!entry) abort();
    entry->next = NULL; entry->size = size; memcpy(entry->bytes, bytes, size);
    if (owner->inbox_tail) owner->inbox_tail->next = entry; else owner->inbox = entry;
    owner->inbox_tail = entry;
}
/* Copies the oldest entry into `out` and frees it; 0 when empty. */
int8_t tarn_rt_async_inbox_pop(TarnExecution *owner, void *out, uint64_t size) {
    TarnSpawned *entry = owner->inbox;
    if (!entry) return 0;
    if (entry->size != size) tarn_rt_fault();
    owner->inbox = entry->next;
    if (!owner->inbox) owner->inbox_tail = NULL;
    memcpy(out, entry->bytes, size); free(entry);
    return 1;
}

/* Phase 15B syscall/ABI bridge. High-level retries and ownership are Tarn.
 * Domain 3 distinguishes filesystem errno from the networking categories. */
#include <dirent.h>
#include <sys/stat.h>
static void fs_error(TarnNetRaw *out, int code) { out->domain = 3; out->code = code; }
static void fs_status(TarnNetRaw *out, int64_t value) {
    net_init(out); out->value = value;
    if (value < 0) fs_error(out, errno);
}
static void fs_trace(const char *event, int fd) {
    if (TARN_TRACE(FS)) { flockfile(stderr); fprintf(stderr, "fs:%s:%d\n", event, fd); funlockfile(stderr); }
}
static char *fs_path(TarnNetRaw *out, const TarnString *path) {
    net_init(out);
    if (memchr(path->bytes, 0, path->len)) { fs_error(out, EINVAL); return NULL; }
    char *text = malloc(path->len + 1);
    if (!text) abort();
    if (path->len) memcpy(text, path->bytes, path->len);
    text[path->len] = 0;
    return text;
}
void tarn_rt_fs_open(TarnNetRaw *out, const TarnString *path, int32_t mode, int32_t permissions) {
    if (permissions < 0 || permissions > 0777) { net_init(out); fs_error(out, EINVAL); return; }
    char *text = fs_path(out, path); if (!text) return;
    int flags;
    switch (mode) {
        case 0: flags = O_RDONLY; break;
        case 1: flags = O_RDWR | O_CREAT | O_TRUNC; break;
        case 2: flags = O_WRONLY | O_CREAT | O_APPEND; break;
        case 3: flags = O_RDWR; break;
        case 4: flags = O_RDWR | O_CREAT | O_EXCL; break;
        default: free(text); fs_error(out, EINVAL); return;
    }
    /* O_NONBLOCK prevents opening a FIFO from blocking before Tarn rejects
     * non-regular resources. It has no effect on regular-file operations. */
    fs_status(out, open(text, flags | O_CLOEXEC | O_NONBLOCK, (mode_t)permissions));
    free(text);
    if (!out->code) fs_trace("open", (int)out->value);
}
void tarn_rt_fs_read(TarnNetRaw *out, int32_t fd, uint8_t *bytes, uint64_t len) {
    net_init(out); if (!len) return;
    fs_status(out, read(fd, bytes, len > INT64_MAX ? INT64_MAX : len));
}
void tarn_rt_fs_write(TarnNetRaw *out, int32_t fd, const uint8_t *bytes, uint64_t len) {
    net_init(out); if (!len) return;
    fs_status(out, write(fd, bytes, len > INT64_MAX ? INT64_MAX : len));
}
void tarn_rt_fs_seek(TarnNetRaw *out, int32_t fd, int64_t offset, int32_t origin) { fs_status(out, lseek(fd, offset, origin)); }
static void fs_info(TarnNetRaw *out, const struct stat *info) {
    if (info->st_size < 0) { fs_error(out, EOVERFLOW); return; }
    out->value = info->st_size;
    out->address.bytes[0] = S_ISREG(info->st_mode) ? 1 : S_ISDIR(info->st_mode) ? 2 : 3;
}
void tarn_rt_fs_file_metadata(TarnNetRaw *out, int32_t fd) {
    struct stat info; fs_status(out, fstat(fd, &info)); if (!out->code) fs_info(out, &info);
}
void tarn_rt_fs_metadata(TarnNetRaw *out, const TarnString *path) {
    char *text = fs_path(out, path); if (!text) return;
    struct stat info; fs_status(out, stat(text, &info)); free(text); if (!out->code) fs_info(out, &info);
}
void tarn_rt_fs_sync(TarnNetRaw *out, int32_t fd) { fs_status(out, fsync(fd)); }
void tarn_rt_fs_close(TarnNetRaw *out, int32_t fd) {
    fs_trace("close", fd); fs_status(out, close(fd));
    if (out->code == EBADF) abort();
    /* Linux releases a valid descriptor even on EINTR; never retry close. */
}
void tarn_rt_fs_drop(int32_t fd) { TarnNetRaw out; tarn_rt_fs_close(&out, fd); }
void tarn_rt_fs_mkdir(TarnNetRaw *out, const TarnString *path, int32_t permissions) {
    if (permissions < 0 || permissions > 0777) { net_init(out); fs_error(out, EINVAL); return; }
    char *text = fs_path(out, path); if (!text) return;
    fs_status(out, mkdir(text, (mode_t)permissions)); free(text);
}
void tarn_rt_fs_remove_file(TarnNetRaw *out, const TarnString *path) {
    char *text = fs_path(out, path); if (!text) return;
    fs_status(out, unlink(text)); free(text);
}
void tarn_rt_fs_remove_dir(TarnNetRaw *out, const TarnString *path) {
    char *text = fs_path(out, path); if (!text) return;
    fs_status(out, rmdir(text)); free(text);
}
void tarn_rt_fs_rename(TarnNetRaw *out, const TarnString *from, const TarnString *to) {
    char *a = fs_path(out, from); if (!a) return;
    char *b = fs_path(out, to); if (!b) { free(a); return; }
    fs_status(out, rename(a, b)); free(a); free(b);
}
void tarn_rt_fs_dir_open(TarnNetRaw *out, const TarnString *path) {
    char *text = fs_path(out, path); if (!text) return;
    DIR *directory = opendir(text);
    if (!directory) fs_error(out, errno);
    else { out->value = (int64_t)(uintptr_t)directory; fs_trace("dir_open", dirfd(directory)); }
    free(text);
}
void tarn_rt_fs_dir_next(TarnNetRaw *out, DIR *directory, uint8_t *bytes, uint64_t capacity) {
    net_init(out); errno = 0;
    struct dirent *entry = readdir(directory);
    if (!entry) { if (errno) fs_error(out, errno); else out->value = -1; return; }
    uint64_t len = strlen(entry->d_name);
    if (len > capacity) { fs_error(out, ENAMETOOLONG); return; }
    if (len) memcpy(bytes, entry->d_name, len);
    out->value = (int64_t)len;
}
void tarn_rt_fs_dir_drop(DIR *directory) {
    fs_trace("dir_close", dirfd(directory));
    if (closedir(directory) < 0 && errno == EBADF) abort();
}

/* Phase 15D: syscall/ABI bridge. Configuration, capture accumulation and
 * ownership are Tarn. No runtime owner bit or child registry. */
#include <spawn.h>
#include <signal.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <poll.h>
extern char **environ;
static void process_trace(const char *event, int id) {
    if (TARN_TRACE(PROCESS)) { flockfile(stderr); fprintf(stderr, "process:%s:%d\n", event, id); funlockfile(stderr); }
}
static void process_error(TarnNetRaw *out, int code) { net_init(out); out->domain = 4; out->code = code; }
static void process_close(int fd) {
    process_trace("fd_close", fd);
    net_timer_forget(fd); /* readiness identity (ADR 0071), before fd reuse */
    if (close(fd) < 0 && errno == EBADF) abort(); /* Never retry Linux close. */
}
static char *process_text(TarnNetRaw *out, const TarnString *value) {
    if (memchr(value->bytes, 0, value->len)) { process_error(out, EINVAL); return NULL; }
    char *text = malloc(value->len + 1); if (!text) abort();
    memcpy(text, value->bytes, value->len); text[value->len] = 0; return text;
}
static int process_fd(int fd) {
    if (fd < 0) return fd;
    if (fd < 3) {
        int next = fcntl(fd, F_DUPFD_CLOEXEC, 3), code = errno;
        (void)close(fd); errno = code; fd = next;
        if (fd < 0) return -1;
    }
    process_trace("fd_open", fd); return fd;
}
static int process_pipe(int fds[2]) {
    int pair[2];
    if (pipe2(pair, O_CLOEXEC) < 0) return errno;
    int first = process_fd(pair[0]);
    if (first < 0) { int code = errno; (void)close(pair[1]); return code; }
    int second = process_fd(pair[1]);
    if (second < 0) { int code = errno; process_close(first); return code; }
    fds[0] = first; fds[1] = second; return 0;
}
/* Inherited environment with `KEY=value` overrides; a later override of the
 * same key wins. NULL with out->code set on invalid entries. */
static char **process_environment(TarnNetRaw *out, const TarnString *const *entries, uint64_t count) {
    uint64_t inherited = 0; while (environ[inherited]) ++inherited;
    if (count > SIZE_MAX / sizeof(char *) - inherited - 1) { process_error(out, E2BIG); return NULL; }
    char **envp = calloc(inherited + count + 1, sizeof(char *)); if (!envp) abort();
    uint64_t used = 0;
    for (uint64_t i = 0; i < count; ++i) {
        const char *eq = memchr(entries[i]->bytes, '=', entries[i]->len);
        if (!eq || eq == (const char *)entries[i]->bytes) { process_error(out, EINVAL); goto fail; }
        size_t key = (size_t)(eq - (const char *)entries[i]->bytes);
        int later = 0;
        for (uint64_t j = i + 1; j < count && !later; ++j)
            later = entries[j]->len > key && !memcmp(entries[j]->bytes, entries[i]->bytes, key + 1);
        if (later) continue;
        envp[used] = process_text(out, entries[i]); if (!envp[used]) goto fail; ++used;
    }
    for (uint64_t i = 0; i < inherited; ++i) {
        int replaced = 0;
        for (uint64_t j = 0; j < used && !replaced; ++j) {
            size_t key = (size_t)(strchr(envp[j], '=') - envp[j]);
            replaced = !strncmp(environ[i], envp[j], key + 1);
        }
        if (replaced) continue;
        envp[used] = strdup(environ[i]); if (!envp[used]) abort(); ++used;
    }
    return envp;
fail:
    for (uint64_t i = 0; i < used; ++i) free(envp[i]);
    free(envp); return NULL;
}
void tarn_rt_process_spawn(TarnNetRaw *out, const TarnString *program,
        const TarnString *const *arguments, uint64_t count,
        const TarnString *const *environment, uint64_t environment_count, const TarnString *directory,
        uint8_t use_directory, uint8_t capture, int32_t stdio, const int32_t *passed, uint64_t passed_count) {
    net_init(out);
    if (!program->len || (use_directory && !directory->len)) { process_error(out, EINVAL); return; }
    /* ADR 0070: at most 64 handed-over descriptors, child fds 3.. in order. */
    if (passed_count > 64 || stdio < 0 || stdio > 63 || (capture && stdio)) { process_error(out, EINVAL); return; }
    for (int target = 0; target < 3; ++target) if (((stdio >> (2 * target)) & 3) == 3) { process_error(out, EINVAL); return; }
    if (count > SIZE_MAX / sizeof(char *) - 2) { process_error(out, E2BIG); return; }
    char **envp = environ;
    if (environment_count) { envp = process_environment(out, environment, environment_count); if (!envp) return; }
    char **argv = calloc(count + 2, sizeof(char *)); if (!argv) abort();
    char *cwd = NULL; int code = 0, pipes[6] = {-1, -1, -1, -1, -1, -1}, input = -1, null = -1;
    int handoff[64]; for (int i = 0; i < 64; ++i) handoff[i] = -1;
    posix_spawn_file_actions_t actions; posix_spawnattr_t attributes;
    int actions_ready = 0, attributes_ready = 0; pid_t pid = -1;
    argv[0] = process_text(out, program); if (!argv[0]) { code = out->code; goto done; }
    for (uint64_t i = 0; i < count; ++i) {
        argv[i + 1] = process_text(out, arguments[i]);
        if (!argv[i + 1]) { code = out->code; goto done; }
    }
    if (use_directory) { cwd = process_text(out, directory); if (!cwd) { code = out->code; goto done; } }
    code = posix_spawn_file_actions_init(&actions); if (code) goto done; actions_ready = 1;
    code = posix_spawnattr_init(&attributes); if (code) goto done; attributes_ready = 1;
    sigset_t empty, defaults; sigemptyset(&empty); sigemptyset(&defaults); sigaddset(&defaults, SIGPIPE);
    code = posix_spawnattr_setsigmask(&attributes, &empty); if (code) goto done;
    code = posix_spawnattr_setsigdefault(&attributes, &defaults); if (code) goto done;
    code = posix_spawnattr_setflags(&attributes, POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF); if (code) goto done;
    if (capture) {
        code = process_pipe(pipes); if (code) goto done;
        code = process_pipe(pipes + 2); if (code) goto done;
        input = process_fd(open("/dev/null", O_RDONLY | O_CLOEXEC)); if (input < 0) { code = errno; goto done; }
        code = posix_spawn_file_actions_adddup2(&actions, input, 0); if (code) goto done;
        code = posix_spawn_file_actions_adddup2(&actions, pipes[1], 1); if (code) goto done;
        code = posix_spawn_file_actions_adddup2(&actions, pipes[3], 2); if (code) goto done;
    } else if (stdio) {
        /* Two bits per stream (stdin, stdout, stderr): 1 /dev/null, 2 pipe.
         * Pipe slots: stdout 0/1, stderr 2/3, stdin 4/5 (read/write ends). */
        static const int slot[3] = {4, 0, 2};
        for (int target = 0; target < 3; ++target) {
            int mode = (stdio >> (2 * target)) & 3;
            if (mode == 1) {
                if (null < 0) { null = process_fd(open("/dev/null", O_RDWR | O_CLOEXEC)); if (null < 0) { code = errno; goto done; } }
                code = posix_spawn_file_actions_adddup2(&actions, null, target); if (code) goto done;
            } else if (mode == 2) {
                code = process_pipe(pipes + slot[target]); if (code) goto done;
                int child_end = pipes[slot[target] + (target == 0 ? 0 : 1)];
                code = posix_spawn_file_actions_adddup2(&actions, child_end, target); if (code) goto done;
            }
        }
    }
    /* Each handed-over descriptor is first duplicated above every target, so
     * no dup2 can overwrite a later source; dup2 clears close-on-exec only on
     * the child's copy. The parent owners stay untouched and close normally. */
    for (uint64_t i = 0; i < passed_count; ++i) {
        handoff[i] = fcntl(passed[i], F_DUPFD_CLOEXEC, 3 + (int)passed_count);
        if (handoff[i] < 0) { code = errno; goto done; }
        process_trace("fd_open", handoff[i]);
        code = posix_spawn_file_actions_adddup2(&actions, handoff[i], 3 + (int)i); if (code) goto done;
    }
    if (use_directory) { code = posix_spawn_file_actions_addchdir_np(&actions, cwd); if (code) goto done; }
    /* Child never inherits other application/runtime descriptors. GNU libc
     * closefrom/chdir spawn actions avoid unsafe post-fork Tarn callbacks. */
    code = posix_spawn_file_actions_addclosefrom_np(&actions, 3 + (int)passed_count); if (code) goto done;
    /* Runtime print buffering is hidden state, not a program-owned buffer:
     * flush it so an inheriting child cannot overtake earlier prints. */
    (void)fflush(stdout);
    code = posix_spawnp(&pid, argv[0], &actions, &attributes, argv, envp);
    if (!code) {
        net_init(out); out->value = pid; process_trace("spawn", pid);
        /* Parent ends pass to Tarn owners: stdout read, stderr read, stdin write. */
        uint8_t *bytes = (uint8_t *)&out->address;
        for (int i = 0; i < 4; ++i) {
            bytes[i] = (uint32_t)pipes[0] >> (i * 8); bytes[4 + i] = (uint32_t)pipes[2] >> (i * 8);
            bytes[8 + i] = (uint32_t)pipes[5] >> (i * 8);
        }
        /* Piped child ends get a unique readiness identity, like timers, so
         * Poll tokens cannot confuse a recycled descriptor (ADR 0035). */
        if (!capture) for (int i = 0; i < 6; i += 1) if ((i == 0 || i == 2 || i == 5) && pipes[i] >= 0) net_timer_track(pipes[i]);
        pipes[0] = -1; pipes[2] = -1; pipes[5] = -1;
    }
done:
    if (actions_ready) posix_spawn_file_actions_destroy(&actions);
    if (attributes_ready) posix_spawnattr_destroy(&attributes);
    for (int i = 0; i < 6; ++i) if (pipes[i] >= 0) process_close(pipes[i]);
    if (input >= 0) process_close(input);
    if (null >= 0) process_close(null);
    for (int i = 0; i < 64; ++i) if (handoff[i] >= 0) process_close(handoff[i]);
    for (uint64_t i = 0; i < count + 1; ++i) free(argv[i]);
    free(argv); free(cwd);
    if (envp != environ) { for (char **e = envp; *e; ++e) free(*e); free(envp); }
    if (code) process_error(out, code);
}
void tarn_rt_process_wait(TarnNetRaw *out, int32_t pid) {
    process_trace("wait", pid); /* One completion attempt, even on an OS error. */
    net_init(out); if (pid <= 0) { process_error(out, EINVAL); return; }
    int status; pid_t result;
    do { result = waitpid(pid, &status, 0); } while (result < 0 && errno == EINTR);
    if (result < 0) { process_error(out, errno); return; }
    process_trace("reap", pid);
    if (WIFEXITED(status)) { out->value = WEXITSTATUS(status); }
    else if (WIFSIGNALED(status)) { out->value = WTERMSIG(status); ((uint8_t *)&out->address)[0] = 1; }
    else abort();
}
/* ADR 0070: observe completion without reaping (WNOWAIT), so the pid stays
 * owned by Process until its consuming wait or destruction. A positive
 * timeout waits on a pidfd; EINTR resumes with the remaining time. Result:
 * address[1] = 1 when finished, address[0] = 1 when signaled. */
void tarn_rt_process_poll_exit(TarnNetRaw *out, int32_t pid, int32_t timeout) {
    net_init(out);
    if (pid <= 0 || timeout < 0) { process_error(out, EINVAL); return; }
    if (timeout > 0) {
        int fd = (int)syscall(SYS_pidfd_open, pid, 0);
        if (fd < 0) { process_error(out, errno); return; }
        struct timespec start; clock_gettime(CLOCK_MONOTONIC, &start);
        int remaining = timeout;
        for (;;) {
            struct pollfd entry = {fd, POLLIN, 0};
            if (poll(&entry, 1, remaining) >= 0) break;
            if (errno != EINTR) { int code = errno; (void)close(fd); process_error(out, code); return; }
            struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
            int64_t spent = (int64_t)(now.tv_sec - start.tv_sec) * 1000 + (now.tv_nsec - start.tv_nsec) / 1000000;
            remaining = spent >= timeout ? 0 : timeout - (int)spent;
        }
        (void)close(fd); /* pidfd is private to this call; never retried */
    }
    siginfo_t info; memset(&info, 0, sizeof(info)); int result;
    do { result = waitid(P_PID, (id_t)pid, &info, WEXITED | WNOHANG | WNOWAIT); } while (result < 0 && errno == EINTR);
    if (result < 0) { process_error(out, errno); return; }
    if (info.si_pid == 0) return;
    uint8_t *bytes = (uint8_t *)&out->address;
    bytes[1] = 1; out->value = info.si_status;
    if (info.si_code != CLD_EXITED) bytes[0] = 1;
}
/* ADR 0072: an owned pidfd for async completion. It gets a readiness
 * identity like piped ends and closes through the ordinary pipe owner. */
void tarn_rt_process_pidfd(TarnNetRaw *out, int32_t pid) {
    net_init(out); if (pid <= 0) { process_error(out, EINVAL); return; }
    int fd = (int)syscall(SYS_pidfd_open, pid, 0);
    if (fd < 0) { process_error(out, errno); return; }
    fd = process_fd(fd); if (fd < 0) { process_error(out, errno); return; }
    net_timer_track(fd); out->value = fd;
}
void tarn_rt_process_drop(int32_t pid) { TarnNetRaw out; tarn_rt_process_wait(&out, pid); }
void tarn_rt_process_pipe_drop(int32_t fd) { process_close(fd); }
void tarn_rt_process_close(TarnNetRaw *out, int32_t fd) { net_init(out); process_close(fd); }
void tarn_rt_process_read(TarnNetRaw *out, int32_t fd, uint8_t *bytes, uint64_t len) {
    net_init(out); ssize_t result = read(fd, bytes, len); out->value = result;
    if (result < 0) process_error(out, errno);
}
void tarn_rt_process_terminate(TarnNetRaw *out, int32_t pid) {
    net_init(out);
    if (pid <= 0) { process_error(out, EINVAL); return; }
    if (kill(pid, SIGTERM) < 0) process_error(out, errno);
}
void tarn_rt_process_kill(TarnNetRaw *out, int32_t pid) {
    net_init(out);
    if (pid <= 0) { process_error(out, EINVAL); return; }
    if (kill(pid, SIGKILL) < 0) process_error(out, errno);
}

/* Private Phase-19 harness ABI; ordinary Tarn programs gain no argv API. */
#include <sys/prctl.h>
int32_t tarn_rt_test_select(int argc, char **argv) {
    /* Disable core collection after exec: piped Linux core handlers otherwise
     * delay abort completion and can turn a panic into a spurious timeout. */
    if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0) abort();
    if (argc != 2 || !argv[1][0]) abort();
    uint64_t value = 0;
    for (const unsigned char *p = (unsigned char *)argv[1]; *p; ++p) {
        if (*p < '0' || *p > '9' || value > ((uint64_t)INT32_MAX - (uint64_t)(*p - '0')) / 10) abort();
        value = value * 10 + (*p - '0');
    }
    return (int32_t)value;
}

/* Non-owning console writes: turn this call's SIGPIPE into an ordinary errno
 * without changing process-wide disposition or consuming a previously pending
 * signal. The mask is thread-local and restored before returning. */
static int64_t tarn_console_output(int32_t fd, const uint8_t *data, uint64_t count, int flush) {
    sigset_t blocked, old, pending;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGPIPE);
    int rc = pthread_sigmask(SIG_BLOCK, &blocked, &old);
    if (rc != 0) { errno = rc; return -1; }
    if (sigpending(&pending) != 0) {
        int saved = errno;
        pthread_sigmask(SIG_SETMASK, &old, NULL);
        errno = saved;
        return -1;
    }
    int already_pending = sigismember(&pending, SIGPIPE);
    int64_t result = flush ? (int64_t)fflush(NULL) : (int64_t)write(fd, data, (size_t)count);
    int saved = errno;
    /* A pipe write that loses its reader midway returns a partial count and
     * still raises SIGPIPE, so consume any SIGPIPE this call generated, not
     * only the EPIPE case (ADR 0073 regression). */
    sigset_t after;
    if (!already_pending && !sigismember(&old, SIGPIPE) && sigpending(&after) == 0 && sigismember(&after, SIGPIPE)) {
        struct timespec zero = {0, 0};
        while (sigtimedwait(&blocked, NULL, &zero) < 0 && errno == EINTR) {}
    }
    pthread_sigmask(SIG_SETMASK, &old, NULL);
    errno = saved;
    return result;
}
/* Child stdin writes (ADR 0071) reuse the console SIGPIPE discipline: a
 * closed reader is EPIPE, never a process-killing signal. */
void tarn_rt_process_write(TarnNetRaw *out, int32_t fd, const uint8_t *bytes, uint64_t len) {
    net_init(out); int64_t result = tarn_console_output(fd, bytes, len, 0); out->value = result;
    if (result < 0) process_error(out, errno);
}
int64_t tarn_console_write(int32_t fd, const uint8_t *data, uint64_t count) {
    return tarn_console_output(fd, data, count, 0);
}
int32_t tarn_console_flush(void) { return (int32_t)tarn_console_output(-1, NULL, 0, 1); }

/* Optional 64-bit Linux libcurl ABI adapter. No native owner escapes a call.
 * Stable option numbers: curl 7.88.1 include/curl/curl.h (curl.se).
 * Loaded lazily: applications not using HTTPS need no libcurl installation.
 */
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <limits.h>
struct tarn_curl_api {
    void *library;
    void *(*init)(void);
    int (*global_init)(long);
    int (*setopt)(void *, int, ...);
    int (*perform)(void *);
    int (*getinfo)(void *, int, ...);
    void (*cleanup)(void *);
    void *(*append)(void *, const char *);
    void (*free_headers)(void *);
    int (*send)(void *, const void *, size_t, size_t *);
    int (*recv)(void *, void *, size_t, size_t *);
    int ready;
};
static struct tarn_curl_api tarn_curl;
static pthread_once_t tarn_curl_once = PTHREAD_ONCE_INIT;
static void tarn_curl_load(void) {
    tarn_curl.library = dlopen("libcurl.so.4", RTLD_NOW | RTLD_LOCAL);
    if (!tarn_curl.library) return;
#define LOAD(field, symbol) do { *(void **)(&tarn_curl.field) = dlsym(tarn_curl.library, symbol); if (!tarn_curl.field) return; } while (0)
    LOAD(init, "curl_easy_init"); LOAD(global_init, "curl_global_init");
    LOAD(setopt, "curl_easy_setopt"); LOAD(perform, "curl_easy_perform");
    LOAD(getinfo, "curl_easy_getinfo"); LOAD(cleanup, "curl_easy_cleanup");
    LOAD(append, "curl_slist_append"); LOAD(free_headers, "curl_slist_free_all");
#undef LOAD
    /* Optional for one-shot HTTPS; required only by persistent TLS streams. */
    *(void **)(&tarn_curl.send) = dlsym(tarn_curl.library, "curl_easy_send");
    *(void **)(&tarn_curl.recv) = dlsym(tarn_curl.library, "curl_easy_recv");
    tarn_curl.ready = tarn_curl.global_init(3L) == 0;
    /* Process-lifetime library/global state; never unload beneath another task. */
}
struct tarn_https_buffer { uint8_t *bytes; size_t length, limit; int exceeded; };
static size_t tarn_https_write(char *bytes, size_t size, size_t count, void *context) {
    struct tarn_https_buffer *out = context;
    if (size && count > SIZE_MAX / size) { out->exceeded = 1; return 0; }
    size_t length = size * count;
    if (length > out->limit - out->length) { out->exceeded = 1; return 0; }
    if (length) memcpy(out->bytes + out->length, bytes, length);
    out->length += length;
    return length;
}
/* Inputs remain live only for this synchronous call. Caller owns the output
 * buffer and both result scalars. -1 unavailable, -2 bound, -3 bad ABI input;
 * positive values preserve libcurl error codes. HTTP statuses are not errors.
 */
int32_t tarn_https_request(const char *url, const char *method, const char *header,
    const uint8_t *body, uint64_t body_size, const char *ca,
    uint64_t timeout, uint8_t *buffer, uint64_t limit,
    uint64_t *used, int64_t *status) {
    *used = 0; *status = 0;
    if (timeout == 0 || timeout > LONG_MAX || body_size > LONG_MAX || limit > SIZE_MAX) return -3;
    pthread_once(&tarn_curl_once, tarn_curl_load);
    if (!tarn_curl.ready) return -1;
    void *handle = tarn_curl.init();
    if (!handle) return 27;
    struct tarn_https_buffer output = {buffer, 0, (size_t)limit, 0};
    void *headers = NULL;
    int code = 0;
#define SET(option, value) do { code = tarn_curl.setopt(handle, option, value); if (code) goto done; } while (0)
    /* Policy contract: HTTPS only, no redirects/proxy/cookies, full verification.
     * Protocol restriction also rejects file:// and other libcurl schemes. */
    SET(10002, url); SET(181, 2L); SET(182, 2L);
    SET(52, 0L); SET(64, 1L); SET(81, 2L); SET(99, 1L);
    SET(10004, ""); SET(155, (long)timeout); SET(156, (long)timeout);
    SET(20011, tarn_https_write); SET(10001, &output);
    if (*ca) SET(10065, ca);
    if (*header) {
        headers = tarn_curl.append(NULL, header);
        if (!headers) { code = 27; goto done; }
    }
    void *more = tarn_curl.append(headers, "Content-Type: application/json");
    if (!more) { code = 27; goto done; }
    headers = more;
    SET(10023, headers);
    if (strcmp(method, "POST") == 0) {
        SET(10015, body_size ? (const void *)body : (const void *)""); SET(60, (long)body_size);
    }
    SET(10036, method);
    code = tarn_curl.perform(handle);
    if (!code) {
        long response = 0;
        code = tarn_curl.getinfo(handle, 0x200002, &response);
        *status = response;
    }
done:
    *used = output.length;
    if (output.exceeded) code = -2;
    tarn_curl.cleanup(handle);
    if (headers) tarn_curl.free_headers(headers);
#undef SET
    return code;
}

/* Persistent TLS client streams (ADR 0066). libcurl connect-only mode does
 * DNS, TCP, the handshake and full verification, then moves raw application
 * bytes. The descriptor belongs to the curl handle: Tarn never closes it.
 * Errors use domain 5: positive libcurl codes, -1 unavailable, -3 invalid
 * input, -5 a stream that already failed. CURLE_AGAIN (81) is WouldBlock. */
#include <poll.h>
typedef struct { void *curl; int fd; int failed; } TarnTls;
static int tls_text(const TarnString *text, char **out) {
    if (text->len > 4096 || memchr(text->bytes, 0, text->len)) return 0;
    *out = malloc(text->len + 1); if (!*out) abort();
    memcpy(*out, text->bytes, text->len); (*out)[text->len] = 0;
    return 1;
}
/* libcurl's stable opensocket ABI (curl/curl.h). The callback completes the
 * PostgreSQL SSLRequest preface before curl starts its verified TLS handshake.
 * curl owns a returned descriptor, including all subsequent failure paths. */
struct tarn_curl_sockaddr {
    int family, socktype, protocol;
    unsigned int addrlen;
    struct sockaddr addr;
};
struct tarn_pg_connect { int64_t deadline; int error; };
static int64_t tls_millis(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) < 0) return -1;
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}
static int pg_wait(int fd, short events, struct tarn_pg_connect *ctx) {
    for (;;) {
        int64_t now = tls_millis();
        int64_t left = ctx->deadline - now;
        if (now < 0 || left <= 0) { ctx->error = 28; return 0; }
        struct pollfd pfd = {fd, events, 0};
        int count = poll(&pfd, 1, left > INT_MAX ? INT_MAX : (int)left);
        if (count > 0) return 1;
        if (count == 0) { ctx->error = 28; return 0; }
        if (errno != EINTR) { ctx->error = 7; return 0; }
    }
}
static int pg_open(void *context, int purpose, struct tarn_curl_sockaddr *target) {
    struct tarn_pg_connect *ctx = context;
    if (purpose != 0 || (target->family != AF_INET && target->family != AF_INET6)
        || (target->socktype & ~(SOCK_CLOEXEC | SOCK_NONBLOCK)) != SOCK_STREAM) { ctx->error = -3; return -1; }
    int fd = socket(target->family, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, target->protocol);
    if (fd < 0) { ctx->error = 7; return -1; }
    if (connect(fd, &target->addr, target->addrlen) < 0) {
        if (errno != EINPROGRESS || !pg_wait(fd, POLLOUT, ctx)) goto failure;
        int error = 0; socklen_t len = sizeof(error);
        if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &len) < 0 || error) goto failure;
    }
    static const uint8_t request[8] = {0, 0, 0, 8, 4, 210, 22, 47};
    size_t sent = 0;
    while (sent < sizeof(request)) {
        ssize_t n = send(fd, request + sent, sizeof(request) - sent, MSG_NOSIGNAL);
        if (n > 0) { sent += (size_t)n; continue; }
        if (n < 0 && errno == EINTR) continue;
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) && pg_wait(fd, POLLOUT, ctx)) continue;
        goto failure;
    }
    uint8_t response;
    for (;;) {
        ssize_t n = recv(fd, &response, 1, 0);
        if (n == 1) break;
        if (n < 0 && errno == EINTR) continue;
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) && pg_wait(fd, POLLIN, ctx)) continue;
        goto failure;
    }
    if (response != 'S') { ctx->error = 35; goto failure; }
    ctx->error = 0;
    return fd;
failure:
    if (!ctx->error) ctx->error = 7;
    close(fd);
    return -1;
}
static int pg_connected(void *context, int fd, int purpose) {
    (void)context; (void)fd;
    return purpose == 0 ? 2 : 1; /* CURL_SOCKOPT_ALREADY_CONNECTED */
}

void tarn_rt_net_tls_connect(TarnNetRaw *out, const TarnString *url, const TarnString *ca, int64_t timeout) {
    net_init(out); out->domain = 5;
    char *address = NULL, *trust = NULL;
    int postgres = url->len > 13 && !memcmp(url->bytes, "postgresql://", 13);
    if (timeout <= 0 || timeout > LONG_MAX || url->len < 9
        || (!postgres && memcmp(url->bytes, "https://", 8))
        || !tls_text(url, &address) || !tls_text(ca, &trust)) { out->code = -3; free(address); return; }
    if (postgres) {
        /* Only an authority is accepted: never credentials, path or query. */
        if (!address[13] || strpbrk(address + 13, "/?#@\\")) {
            out->code = -3; free(address); free(trust); return;
        }
        memmove(address + 8, address + 13, strlen(address + 13) + 1);
        memcpy(address, "https://", 8);
    }
    int64_t started = tls_millis();
    if (started < 0 || timeout > INT64_MAX - started) { out->code = -3; free(address); free(trust); return; }
    struct tarn_pg_connect pg = {started + timeout, 0};
    pthread_once(&tarn_curl_once, tarn_curl_load);
    if (!tarn_curl.ready || !tarn_curl.send || !tarn_curl.recv) { out->code = -1; free(address); free(trust); return; }
    void *handle = tarn_curl.init();
    int code = handle ? 0 : 27;
    int socket = -1; /* curl_socket_t is int on Linux, not long */
#define SET(option, value) do { if (!code) code = tarn_curl.setopt(handle, option, value); } while (0)
    /* Same policy as tarn_https_request: HTTPS only, no redirect/proxy, full
     * verification. 141 CONNECT_ONLY, 156 CONNECTTIMEOUT_MS bounds the setup. */
    SET(10002, address); SET(181, 2L); SET(182, 2L);
    SET(52, 0L); SET(64, 1L); SET(81, 2L); SET(99, 1L);
    SET(10004, ""); SET(156, (long)timeout); SET(141, 1L);
    /* 84 HTTP_VERSION = 1.1: the stream carries the caller's own protocol
     * (e.g. a WebSocket upgrade), so ALPN must not negotiate h2. libcurl's
     * default for https offers h2 and servers like api.openai.com pick it,
     * which breaks any HTTP/1.1 upgrade sent on the raw stream. */
    SET(84, 2L);
    if (postgres) {
        SET(226, 0L); /* SSL_ENABLE_ALPN: PostgreSQL preface, no HTTP ALPN. */
        SET(20163, pg_open); SET(10164, &pg);
        SET(20148, pg_connected);
        SET(155, (long)timeout); /* DNS + preface + verified handshake total. */
    }
    if (*trust) SET(10065, trust);
#undef SET
    if (!code) code = tarn_curl.perform(handle);
    if (code && pg.error) code = pg.error;
    if (!code) code = tarn_curl.getinfo(handle, 0x50002C, &socket); /* ACTIVESOCKET */
    if (!code && socket < 0) code = 2;
    free(address); free(trust);
    if (code) { if (handle) tarn_curl.cleanup(handle); out->code = code; return; }
    TarnTls *tls = malloc(sizeof(*tls)); if (!tls) abort();
    tls->curl = handle; tls->fd = socket; tls->failed = 0;
    net_trace("open", tls->fd);
    out->domain = 0; out->value = (int64_t)(uintptr_t)tls;
}
/* One partial transfer. With `wait`, CURLE_AGAIN waits for the descriptor and
 * retries; plaintext already buffered by TLS is always attempted first. */
static void tls_transfer(TarnNetRaw *out, uint64_t handle, uint8_t *bytes, uint64_t len, uint8_t wait, int writing) {
    TarnTls *tls = (TarnTls *)(uintptr_t)handle;
    net_init(out);
    if (tls->failed) { out->domain = 5; out->code = -5; return; }
    if (!len) return;
    for (;;) {
        size_t moved = 0;
        int code = writing ? tarn_curl.send(tls->curl, bytes, (size_t)len, &moved) : tarn_curl.recv(tls->curl, bytes, (size_t)len, &moved);
        if (!code) { out->value = (int64_t)moved; return; }
        if (code != 81) { tls->failed = 1; out->domain = 5; out->code = code; return; }
        if (!wait) { out->domain = 5; out->code = 81; return; }
        struct pollfd ready = { tls->fd, writing ? POLLOUT : POLLIN, 0 };
        if (poll(&ready, 1, -1) < 0 && errno != EINTR) { out->code = errno; return; }
    }
}
void tarn_rt_net_tls_read(TarnNetRaw *out, uint64_t handle, uint8_t *bytes, uint64_t len, uint8_t wait) { tls_transfer(out, handle, bytes, len, wait, 0); }
void tarn_rt_net_tls_write(TarnNetRaw *out, uint64_t handle, const uint8_t *bytes, uint64_t len, uint8_t wait) { tls_transfer(out, handle, (uint8_t *)bytes, len, wait, 1); }
void tarn_rt_net_tls_fd(TarnNetRaw *out, uint64_t handle) { net_init(out); out->value = ((TarnTls *)(uintptr_t)handle)->fd; }
/* Cleanup closes the descriptor exactly once; never retried. */
void tarn_rt_net_tls_drop(uint64_t handle) {
    TarnTls *tls = (TarnTls *)(uintptr_t)handle;
    net_trace("close", tls->fd);
    tarn_curl.cleanup(tls->curl);
    free(tls);
}
void tarn_rt_net_close_tls(TarnNetRaw *out, uint64_t handle) { net_init(out); tarn_rt_net_tls_drop(handle); }

/* Mechanical libc decimal conversion using an explicit C numeric locale.
 * Public grammar, policies and UTF-8 ownership stay in Tarn. */
#include <locale.h>
static locale_t tarn_numeric_locale;
static pthread_once_t tarn_numeric_once = PTHREAD_ONCE_INIT;
static void tarn_numeric_init(void) {
    tarn_numeric_locale = newlocale(LC_NUMERIC_MASK, "C", (locale_t)0);
    if (!tarn_numeric_locale) abort();
}
int32_t tarn_parse_f64(const char *text, double *out) {
    pthread_once(&tarn_numeric_once, tarn_numeric_init);
    char *end;
    double value = strtod_l(text, &end, tarn_numeric_locale);
    if (end == text || *end || !isfinite(value)) return 0;
    *out = value;
    return 1;
}
uint64_t tarn_format_f64(double value, uint8_t *out, uint64_t capacity) {
    pthread_once(&tarn_numeric_once, tarn_numeric_init);
    locale_t previous = uselocale(tarn_numeric_locale);
    if (!previous) abort();
    int count = snprintf((char *)out, capacity, "%.17g", value);
    if (!uselocale(previous)) abort();
    if (count < 0 || (uint64_t)count >= capacity) return 0;
    return (uint64_t)count;
}
