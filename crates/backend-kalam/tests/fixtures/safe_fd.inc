/* Included by kalam_fixture.c only for controlled SAFETY_FD_V2 tests. */
#include <sys/stat.h>
#include <fcntl.h>
#ifndef SAFETY_VERSION
#define SAFETY_VERSION 2
#endif
static unsigned int transfer_calls;
uint32_t SafetyVersion(void) { return SAFETY_VERSION; }
unsigned int FixtureTransferCalls(void) { return transfer_calls; }

static void events(on_cb_result_t *pre, on_cb_result_t *progress) {
    if (pre) send(pre, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"size\":7}}"));
    if (mode == 28) send(progress, owned("not JSON"));
    else if (mode == 29) {
        char *large = malloc(LIMIT + 2);
        if (!large) abort();
        memset(large, 'x', LIMIT + 1);
        large[LIMIT + 1] = 0;
        allocations++;
        send(progress, large);
    } else if (mode == 35) send(progress, NULL);
    else if (mode == 36) send(progress, owned("{\"errorType\":\"ProgressError\",\"error\":\"private event detail\",\"data\":null}"));
    else send(progress, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"sent\":0}}"));
    /* A second event is not a second terminal result. */
    send(progress, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"sent\":7,\"progress\":100}}"));
}

static void transfer_done(on_cb_result_t *done, int upload) {
    const char *normal = upload
        ? "{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":123,\"actualName\":\"report-mtp-0123456789abcdef0123456789abcdef.txt\",\"path\":\"/report-mtp-0123456789abcdef0123456789abcdef.txt\",\"needsReview\":false}}"
        : "{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":42}}";
    switch (mode) {
    case 20: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":6,\"objectId\":42}}")); return;
    case 21: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":false,\"bytes\":7,\"objectId\":42}}")); return;
    case 22: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"needsReview\":true,\"bytes\":7,\"objectId\":42}}")); return;
    case 23: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":0}}")); return;
    case 24: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":4294967295}}")); return;
    case 25: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":123,\"actualName\":\"../report.txt\",\"path\":\"/../report.txt\",\"needsReview\":false}}")); return;
    case 26: send(done, owned("{\"errorType\":\"ErrorNeedsReview\",\"error\":\"private /device/private/report.txt\",\"data\":{\"committed\":false,\"needsReview\":true,\"bytes\":3,\"objectId\":123,\"path\":\"/device/private/report.txt\"}}")); return;
    case 31: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":43}}")); return;
    case 32: send(done, owned(normal)); send(done, owned(normal)); return;
    case 33: return;
    case 34: send(done, owned("not JSON")); return;
    case 37: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"committed\":true,\"bytes\":7,\"objectId\":123,\"actualName\":\"report.txt\",\"path\":\"/report.txt\",\"needsReview\":false}}")); return;
    default: send(done, owned(normal)); return;
    }
}

#ifndef OMIT_SAFE_UPLOAD
void SafeUploadSingle(char *input, int source_fd, on_cb_result_t *pre, on_cb_result_t *progress, on_cb_result_t *done) {
    transfer_calls++;
    if (!strstr(input, "\"storageId\":65537") || !strstr(input, "\"parentId\":null") ||
        !strstr(input, "\"destination\":\"/\"") || !strstr(input, "\"requestedName\":\"report.txt\"") ||
        !strstr(input, "\"policy\":\"keepBoth\"") || !strstr(input, "\"expectedSize\":7") || strstr(input, "\"source\"")) abort();
    int owned_fd = dup(source_fd);
    if (owned_fd < 0) abort();
    struct stat metadata;
    if (fstat(owned_fd, &metadata) || !S_ISREG(metadata.st_mode) || metadata.st_size != 7) abort();
    if (lseek(owned_fd, 0, SEEK_SET) != 0) abort();
    char bytes[8] = {0};
    if (read(owned_fd, bytes, 7) != 7 || memcmp(bytes, "payload", 7)) abort();
    if (close(owned_fd)) abort();
    events(pre, progress);
    transfer_done(done, 1);
}
#endif

#ifndef OMIT_SAFE_DOWNLOAD
void SafeDownloadSingle(char *input, int output_fd, on_cb_result_t *progress, on_cb_result_t *done) {
    transfer_calls++;
    if (!strstr(input, "\"storageId\":65537") || !strstr(input, "\"objectId\":42") || !strstr(input, "\"expectedSize\":7")) abort();
    int owned_fd = dup(output_fd);
    if (owned_fd < 0) abort();
    struct stat metadata;
    if (fstat(owned_fd, &metadata) || !S_ISREG(metadata.st_mode) || metadata.st_size != 0 || (fcntl(owned_fd, F_GETFL) & O_APPEND)) abort();
    if (lseek(owned_fd, 0, SEEK_SET) != 0) abort();
    size_t length = mode == 30 ? 6U : 7U;
    if (write(owned_fd, "payload", length) != (ssize_t)length || close(owned_fd)) abort();
    events(NULL, progress);
    transfer_done(done, 0);
}
#endif
