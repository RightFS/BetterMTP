/* Worker end-to-end fixture only. No USB/device or user-path writes.
 * The mutation regression alone writes an explicitly compiled fixture-owned
 * source path; real transfer requests still contain only remote metadata/FDs.
 * Reuse binding's controlled FD implementation; wrappers adapt directory sizes,
 * activate failure modes only on transfers, and enforce root identity in worker.
 */
#define SAFETY_FD_V2
#define Initialize BindingInitialize
#define Walk BindingWalk
#define SafeUploadSingle BindingUpload
#define SafeDownloadSingle BindingDownload
#include "../../../backend-kalam/tests/fixtures/kalam_fixture.c"
#undef Initialize
#undef Walk
#undef SafeUploadSingle
#undef SafeDownloadSingle

#ifndef WORKER_MODE
#define WORKER_MODE 0
#endif
#ifdef WORKER_EMPTY
#define WORKER_SIZE 0
#else
#define WORKER_SIZE 7
#endif

void Initialize(on_cb_result_t *done) { BindingInitialize(done); }
void Walk(char *input, on_cb_result_t *done) {
    if (!strstr(input, "\"storageId\":65537")) abort();
#ifdef WORKER_NONROOT
    if (strstr(input, "\"fullPath\":\"/\"")) {
        send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"size\":0,\"isFolder\":true,\"name\":\"folder\",\"path\":\"/folder\",\"parentId\":0,\"objectId\":7}]}"));
        return;
    }
    if (!strstr(input, "\"fullPath\":\"/folder\"")) abort();
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"size\":7,\"isFolder\":false,\"name\":\"a.txt\",\"path\":\"/folder/a.txt\",\"parentId\":7,\"objectId\":42}]}"));
#else
    if (!strstr(input, "\"fullPath\":\"/\"")) abort();
#ifdef WORKER_EMPTY
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"size\":0,\"isFolder\":false,\"name\":\"a.txt\",\"path\":\"/a.txt\",\"parentId\":0,\"objectId\":42}]}"));
#else
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"size\":7,\"isFolder\":false,\"name\":\"a.txt\",\"path\":\"/a.txt\",\"parentId\":0,\"objectId\":42}]}"));
#endif
#endif
}

void SafeUploadSingle(char *input, int fd, on_cb_result_t *pre, on_cb_result_t *progress, on_cb_result_t *done) {
#ifdef WORKER_MUTATE_SOURCE
    /* This compile-time path is created and owned exclusively by this test.
     * NOFOLLOW plus inode-kind/link checks keep the regression narrowly scoped.
     * BindingUpload below must still observe "payload" from its staged FD. */
    int original = open(WORKER_MUTATE_SOURCE_PATH, O_WRONLY | O_NOFOLLOW);
    struct stat original_stat;
    if (original < 0 || fstat(original, &original_stat) || !S_ISREG(original_stat.st_mode)
        || original_stat.st_nlink != 1 || original_stat.st_size != 7) abort();
    if (write(original, "changed", 7) != 7 || fsync(original) || close(original)) abort();
#endif
#ifdef WORKER_STALL
    sleep(2);
#endif
#ifdef WORKER_DIE
    _exit(70);
#endif
    FixtureSetMode(WORKER_MODE);
#if defined(WORKER_EMPTY) || defined(WORKER_NONROOT)
    if (!strstr(input, "\"requestedName\":\"report.txt\"") || strstr(input, "\"source\"")) abort();
    struct stat statbuf;
    if (fstat(fd, &statbuf) || statbuf.st_size != WORKER_SIZE) abort();
#ifdef WORKER_NONROOT
    if (!strstr(input, "\"parentId\":7") || !strstr(input, "\"destination\":\"/folder\"")) abort();
    char bytes[8] = {0};
    int duplicate = dup(fd);
    if (duplicate < 0 || read(duplicate, bytes, 7) != 7 || memcmp(bytes, "payload", 7) || close(duplicate)) abort();
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":123,\"actualName\":\"report-mtp-0123456789abcdef0123456789abcdef.txt\",\"path\":\"/folder/report-mtp-0123456789abcdef0123456789abcdef.txt\",\"needsReview\":false}}"));
#else
    if (!strstr(input, "\"parentId\":null") || !strstr(input, "\"expectedSize\":0")) abort();
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":0,\"objectId\":123,\"actualName\":\"report-mtp-0123456789abcdef0123456789abcdef.txt\",\"path\":\"/report-mtp-0123456789abcdef0123456789abcdef.txt\",\"needsReview\":false}}"));
#endif
    (void)pre; (void)progress;
#else
    BindingUpload(input, fd, pre, progress, done);
#endif
}

void SafeDownloadSingle(char *input, int fd, on_cb_result_t *progress, on_cb_result_t *done) {
#ifdef WORKER_STALL
    sleep(2);
#endif
#ifdef WORKER_DIE
    _exit(70);
#endif
#ifdef WORKER_NONROOT
    if (!strstr(input, "\"parentId\":7")) abort();
#else
    if (!strstr(input, "\"parentId\":4294967295")) abort();
#endif
    if (strstr(input, "\"destination\"")) abort();
    FixtureSetMode(WORKER_MODE);
#ifdef WORKER_EMPTY
    if (!strstr(input, "\"expectedSize\":0")) abort();
    struct stat statbuf;
    if (fstat(fd, &statbuf) || statbuf.st_size != 0) abort();
    send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":0,\"objectId\":42}}"));
    (void)progress;
#else
    BindingDownload(input, fd, progress, done);
#endif
}
