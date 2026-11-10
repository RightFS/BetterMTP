/* Controlled test double only: no Go runtime, USB, MTP or real device. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef void (*on_cb_result_t)(char *);
static int mode;
static unsigned int allocations;
#define LIMIT (1024U * 1024U)

void FixtureSetMode(int value) { mode = value; }
unsigned int FixtureAllocations(void) { return allocations; }
void FixturePrint(void) { puts("fixture-native-stdout"); fflush(stdout); }

static char *owned(const char *text) {
    size_t length = strlen(text);
    char *copy = malloc(length + 1);
    if (!copy) abort();
    memcpy(copy, text, length + 1);
    allocations++;
    return copy;
}

/* Reproduce send_to_js exactly: ptr itself is a function address, no *ptr. */
static void send(on_cb_result_t *ptr, char *json) {
    on_cb_result_t callback = (on_cb_result_t)ptr;
    callback(json);
}

static void result(on_cb_result_t *ptr, const char *normal) {
    switch (mode) {
    case 1:
        send(ptr, owned("{\"errorType\":\"FixtureError\",\"error\":\"native failure\",\"data\":null}"));
        return;
    case 2:
        send(ptr, owned(normal));
        send(ptr, owned(normal)); /* Two distinct transferred allocations. */
        return;
    case 3: return;
    case 4: send(ptr, owned("not JSON")); return;
    case 5: send(ptr, NULL); return;
    case 6: {
        char *large = malloc(LIMIT + 2);
        if (!large) abort();
        memset(large, 'x', LIMIT + 1);
        large[LIMIT + 1] = 0;
        allocations++;
        send(ptr, large);
        return;
    }
    case 7: send(ptr, owned("{\"data\":[]}")); return;
    case 8:
        send(ptr, owned("{\"errorType\":\"FixtureError\",\"error\":\"\",\"data\":[]}"));
        return;
    case 9:
        send(ptr, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":false}"));
        return;
    case 10: {
        const char *prefix = "{\"errorType\":\"\",\"error\":\"\",\"data\":[],\"padding\":\"";
        size_t start = strlen(prefix);
        char *exact = malloc(LIMIT + 1);
        if (!exact) abort();
        memcpy(exact, prefix, start);
        memset(exact + start, 'x', LIMIT - start - 2);
        exact[LIMIT - 2] = '"';
        exact[LIMIT - 1] = '}';
        exact[LIMIT] = 0;
        allocations++;
        send(ptr, exact);
        return;
    }
    case 11:
        send(ptr, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":null}"));
        return;
    default: send(ptr, owned(normal)); return;
    }
}

void Initialize(on_cb_result_t *ptr) {
#ifdef FIXTURE_CHATTER
    puts("native-stdout-must-not-enter-control-channel");
    fflush(stdout);
#endif
#ifdef FIXTURE_STALL
    sleep(2);
#endif
    result(ptr, "{\"errorType\":\"\",\"error\":\"\",\"data\":{\"mtpDeviceInfo\":{\"SerialNumber\":\"fixture-only\"},\"usbDeviceInfo\":{\"VendorId\":1}}}");
}
void FetchDeviceInfo(on_cb_result_t *ptr) { Initialize(ptr); }
void FetchStorages(on_cb_result_t *ptr) {
    result(ptr, "{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"StorageId\":65537,\"StorageInfo\":{\"StorageDescription\":\"fixture\"}}]}");
}
#ifndef OMIT_WALK
void Walk(char *input, on_cb_result_t *ptr) {
    if (!strstr(input, "\"storageId\":65537") ||
        !strstr(input, "\"fullPath\":\"/\"") ||
        !strstr(input, "\"recursive\":false") ||
        !strstr(input, "\"skipDisallowedFiles\":true") ||
        !strstr(input, "\"skipHiddenFiles\":false")) {
        send(ptr, owned("{\"errorType\":\"BadInput\",\"error\":\"bad walk input\",\"data\":null}"));
        return;
    }
    result(ptr, "{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"size\":3,\"isFolder\":false,\"dateAdded\":\"fixture-date\",\"name\":\"a.txt\",\"path\":\"/a.txt\",\"parentPath\":\"/\",\"extension\":\"txt\",\"parentId\":0,\"objectId\":42}]}");
}
#endif
void FileExists(char *input, on_cb_result_t *ptr) {
    if (!strstr(input, "\"storageId\":65537") ||
        !strstr(input, "\"Files\":[\"/a.txt\"]")) {
        send(ptr, owned("{\"errorType\":\"BadInput\",\"error\":\"bad exists input\",\"data\":null}"));
        return;
    }
    result(ptr, "{\"errorType\":\"\",\"error\":\"\",\"data\":[{\"fullpath\":\"/a.txt\",\"exists\":true}]}");
}
void Dispose(on_cb_result_t *ptr) {
    result(ptr, "{\"errorType\":\"\",\"error\":\"\",\"data\":true}");
}

#ifdef SAFETY_FD_V2
#include "safe_fd.inc"
#endif
#ifdef SAFETY_INSPECT
#include "inspect.inc"
#endif
