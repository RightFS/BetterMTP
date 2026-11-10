/* Controlled optional count-only inspection fixture. No FD/file/USB IO. */
#ifndef SAFETY_FD_V2
#ifndef SAFETY_VERSION
#define SAFETY_VERSION 2
#endif
uint32_t SafetyVersion(void) { return SAFETY_VERSION; }
#endif
static unsigned int inspection_calls;
unsigned int FixtureInspectionCalls(void) { return inspection_calls; }

#ifndef OMIT_INSPECT
void InspectUploadDestination(char *input, on_cb_result_t *done) {
    inspection_calls++;
    /* Exact three-field JSON: parentId is present even when null; no source,
       name, size, FD or transfer arguments can enter this independent export. */
    if (strcmp(input, "{\"storageId\":65537,\"parentId\":null,\"destination\":\"/\"}") &&
        strcmp(input, "{\"storageId\":65537,\"parentId\":0,\"destination\":\"/\"}") &&
        strcmp(input, "{\"storageId\":65537,\"parentId\":4294967295,\"destination\":\"/\"}") &&
        strcmp(input, "{\"storageId\":65537,\"parentId\":55,\"destination\":\"/tmp\"}")) abort();
    const char *normal = "{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}";
    switch (mode) {
    case 50: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":false,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    case 51: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":false,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    case 52: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":-1,\"existingHandleCount\":100}}")); return;
    case 53: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":18446744073709551616}}")); return;
    case 54: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3.5,\"existingHandleCount\":100}}")); return;
    case 55: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"private/path\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    case 56: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"SafeUploadSingle\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    case 57: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100,\"path\":\"private/path\"}}")); return;
    case 58: send(done, owned("{\"errorType\":\"ErrorGeneral\",\"error\":\"private /tmp/private-name\",\"data\":{\"noWrites\":true,\"complete\":false,\"phase\":\"snapshotExistingHandles\",\"operation\":\"GetObjectHandles(allStoragesAllParents)\",\"destinationChildCount\":3,\"existingHandleCount\":0}}")); return;
    case 59: send(done, owned(normal)); send(done, owned(normal)); return;
    case 60: return;
    case 61: send(done, owned("not JSON")); return;
    case 62: send(done, NULL); return;
    case 63: {
        char *large = malloc(LIMIT + 2);
        if (!large) abort();
        memset(large, 'x', LIMIT + 1);
        large[LIMIT + 1] = 0;
        allocations++;
        send(done, large);
        return;
    }
    case 64: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"path\":\"private/path\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    case 65: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":0,\"existingHandleCount\":18446744073709551615}}")); return;
    case 66: send(done, owned("{\"errorType\":\"\",\"error\":\"\",\"data\":{\"noWrites\":true,\"complete\":true,\"phase\":\"complete\\n\",\"operation\":\"InspectedUploadDestinationReads\",\"destinationChildCount\":3,\"existingHandleCount\":100}}")); return;
    default: send(done, owned(normal)); return;
    }
}
#endif
