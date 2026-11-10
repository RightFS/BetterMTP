// No-device ABI/ownership/serialization smoke test. Not a transfer test.
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

typedef void (*callback)(char *);
// Legacy ABI uses a function address cast to callback*, not &callback.
typedef void (*done_fn)(callback *);
typedef void (*transfer_fn)(char *, callback *, callback *, callback *);
typedef void (*upload_fd_fn)(char *, int, callback *, callback *, callback *);
typedef void (*download_fd_fn)(char *, int, callback *, callback *);
static done_fn dispose_fn;
static atomic_int callbacks, active, overlap, errors;
static void result(char *json) {
    if (atomic_fetch_add(&active, 1) != 0) atomic_store(&overlap, 1);
    if (!json || !strstr(json, "\"data\":true")) atomic_fetch_add(&errors, 1);
    free(json); // callback has sole ownership, copied/consumed before free.
    usleep(100);
    atomic_fetch_add(&callbacks, 1);
    atomic_fetch_sub(&active, 1);
}
static void forbidden(char *json) {
    if (!json || !strstr(json, "ErrorUnsafeOperation")) atomic_fetch_add(&errors, 1);
    free(json);
}
static void *thread_main(void *unused) {
    (void)unused;
    for (int i = 0; i < 100; ++i) dispose_fn((callback *)result);
    return NULL;
}
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!lib) { fprintf(stderr, "%s\n", dlerror()); return 1; }
    uint32_t (*version)(void) = (uint32_t (*)(void))dlsym(lib, "SafetyVersion");
    dispose_fn = (done_fn)dlsym(lib, "Dispose");
    upload_fd_fn safe = (upload_fd_fn)dlsym(lib, "SafeUploadSingle");
    download_fd_fn download = (download_fd_fn)dlsym(lib, "SafeDownloadSingle");
    transfer_fn legacy = (transfer_fn)dlsym(lib, "UploadFiles");
    if (!version || version() != 2 || !dispose_fn || !safe || !download || !legacy || !dlsym(lib,"DownloadFiles")) return 1;
    legacy("{}", NULL, NULL, (callback *)forbidden);
    safe("{\"policy\":\"replace\",\"parentId\":null}", -1, NULL, NULL, (callback *)forbidden);
    // Absent callbacks must free CString in the native shim.
    for (int i = 0; i < 1000; ++i) dispose_fn(NULL);
    pthread_t threads[4];
    for (int i = 0; i < 4; ++i) if (pthread_create(&threads[i], NULL, thread_main, NULL)) return 1;
    for (int i = 0; i < 4; ++i) pthread_join(threads[i], NULL);
    if (atomic_load(&callbacks) != 400 || atomic_load(&overlap) || atomic_load(&errors)) return 1;
    puts("PASS: dlopen, SafetyVersion=2, symbols, 400 serialized callbacks/free, null callbacks, unsafe upload rejection; NO DEVICE TRANSFER TEST");
    // Go runtime remains loaded until process exit; do not dlclose a Go library.
    return 0;
}
