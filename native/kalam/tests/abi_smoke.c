// No-device ABI/ownership/serialization smoke test. Not a transfer test.
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>
#include <errno.h>
#include <sys/stat.h>

typedef void (*callback)(char *);
typedef void (*json_done_fn)(char *, callback *);
// Legacy ABI uses a function address cast to callback*, not &callback.
typedef void (*done_fn)(callback *);
typedef void (*transfer_fn)(char *, callback *, callback *, callback *);
typedef void (*upload_fd_fn)(char *, int, callback *, callback *, callback *);
typedef void (*download_fd_fn)(char *, int, callback *, callback *);
static done_fn dispose_fn;
static atomic_int callbacks, active, overlap, errors, unsupported_calls, inspection_calls;
static const char *inspection_phase;
static const char *inspection_operation;
static void inspect_result(char *json) {
    char phase[128], operation[192];
    snprintf(phase,sizeof(phase),"\"phase\":\"%s\"",inspection_phase);
    snprintf(operation,sizeof(operation),"\"operation\":\"%s\"",inspection_operation);
    if (!json || strlen(json)>1024 || !strstr(json,"\"noWrites\":true") || !strstr(json,"\"complete\":false") || !strstr(json,phase) || !strstr(json,operation) || !strstr(json,"\"destinationChildCount\":0") || !strstr(json,"\"existingHandleCount\":0") || strstr(json,"\"actualName\"") || strstr(json,"\"path\"") || strstr(json,"\"objectId\"")) atomic_fetch_add(&errors,1);
    atomic_fetch_add(&inspection_calls,1);
    free(json);
}
static void unsupported(char *json) {
    if (!json || !strstr(json, "\"errorType\":\"ErrorUnsupported\"") || !strstr(json,"\"data\":null")) atomic_fetch_add(&errors,1);
    atomic_fetch_add(&unsupported_calls,1);
    free(json);
}
static void unexpected_progress(char *json) { atomic_fetch_add(&errors,1); free(json); }
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
static void upload_validation_diagnostic(char *json) {
    if (!json || !strstr(json,"ErrorUnsafeOperation") || !strstr(json,"\"phase\":\"validateInput\"") || !strstr(json,"\"operation\":\"validateUploadInput\"")) atomic_fetch_add(&errors,1);
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
    transfer_fn legacy_download = (transfer_fn)dlsym(lib,"DownloadFiles");
    json_done_fn legacy_mkdir = (json_done_fn)dlsym(lib,"MakeDirectory");
    json_done_fn inspect = (json_done_fn)dlsym(lib,"InspectUploadDestination");
    if (!version || version() != 2 || !dispose_fn || !safe || !download || !legacy || !legacy_download || !legacy_mkdir || !inspect) return 1;
    inspection_phase="decodeInput"; inspection_operation="json.Unmarshal";
    inspect("{}",(callback *)inspect_result);
    inspection_phase="verifySession"; inspection_operation="FetchDeviceInfoAndVerifySession";
    // Container was never initialized: this stops before any device SDK call.
    inspect("{\"storageId\":1,\"parentId\":null,\"destination\":\"/\"}",(callback *)inspect_result);
    char oversized_input[5001]; memset(oversized_input,'x',5000); oversized_input[5000]=0;
    inspection_phase="decodeInput"; inspection_operation="BoundedInputJson";
    inspect(oversized_input,(callback *)inspect_result);
    if (atomic_load(&inspection_calls)!=3) return 1;
    char fixture[]=".cache/kalam/arm64/no-write-XXXXXX";
    if (!mkdtemp(fixture)) return 1;
    char sentinel[512], child[512], download_json[1024], mkdir_json[1024];
    snprintf(sentinel,sizeof(sentinel),"%s/sentinel",fixture);
    snprintf(child,sizeof(child),"%s/must-not-exist",fixture);
    FILE *f=fopen(sentinel,"wb");
    if (!f || fwrite("DO NOT TOUCH",1,12,f)!=12 || fclose(f)) return 1;
    snprintf(download_json,sizeof(download_json),"{\"storageId\":1,\"sources\":[\"/sentinel\"],\"destination\":\"%s\",\"preprocessFiles\":true}",fixture);
    snprintf(mkdir_json,sizeof(mkdir_json),"{\"storageId\":1,\"fullPath\":\"%s\"}",child);
    legacy_download(download_json,(callback *)unexpected_progress,(callback *)unexpected_progress,(callback *)unsupported);
    legacy_mkdir(mkdir_json,(callback *)unsupported);
    char preserved[16]={0};
    f=fopen(sentinel,"rb");
    if (!f || fread(preserved,1,sizeof(preserved),f)!=12 || fclose(f) || memcmp(preserved,"DO NOT TOUCH",12)) return 1;
    struct stat child_stat;
    if (stat(child,&child_stat)==0 || errno!=ENOENT || atomic_load(&unsupported_calls)!=2) return 1;
    legacy("{}", NULL, NULL, (callback *)forbidden);
    safe("{\"policy\":\"replace\",\"parentId\":null}", -1, NULL, NULL, (callback *)upload_validation_diagnostic);
    // Absent callbacks must free CString in the native shim.
    for (int i = 0; i < 1000; ++i) dispose_fn(NULL);
    pthread_t threads[4];
    for (int i = 0; i < 4; ++i) if (pthread_create(&threads[i], NULL, thread_main, NULL)) return 1;
    for (int i = 0; i < 4; ++i) pthread_join(threads[i], NULL);
    if (atomic_load(&callbacks) != 400 || atomic_load(&overlap) || atomic_load(&errors)) return 1;
    puts("PASS: dlopen, SafetyVersion=2, symbols, 400 serialized callbacks/free, null callbacks, unsafe upload rejection and phase metadata, legacy DownloadFiles/MakeDirectory Unsupported and NoWrite, InspectUploadDestination bounded no-device results; NO DEVICE TRANSFER TEST");
    // Go runtime remains loaded until process exit; do not dlclose a Go library.
    return 0;
}
