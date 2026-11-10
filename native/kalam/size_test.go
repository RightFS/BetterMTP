package main

import (
	"github.com/ganeshrvel/go-mtpfs/mtp"
	"strings"
	"testing"
)

func TestUnsupportedUnsignedSizeIsNotTruncated(t *testing.T) {
	upload := SafeUploadInput{StorageId: 1, Destination: "/", RequestedName: "x", Policy: "keepBoth", ExpectedSize: 1 << 63}
	if err := validateUploadInput(upload); err == nil || !strings.Contains(err.Error(), "ErrorUnsupportedSize") {
		t.Fatalf("%v", err)
	}
	download := SafeDownloadInput{StorageId: 1, ObjectId: 99, ExpectedSize: 1 << 63}
	if _, err := safeDownload(nil, download, -1, nil); err == nil || !strings.Contains(err.Error(), "ErrorUnsupportedSize") {
		t.Fatalf("%v", err)
	}
}
func TestLargeMetadataUsesActualSizeNotCompressedSentinel(t *testing.T) {
	// Metadata-only mock: not a >4GiB transfer or a device property-query test.
	obj := &mtp.ObjectInfo{StorageID: 1, ObjectFormat: mtp.OFC_Undefined, CompressedSize: 0xffffffff}
	i := SafeDownloadInput{StorageId: 1, ObjectId: 99, ExpectedSize: 1 << 32}
	if !downloadMetadataMatches(i, obj, 1<<32) {
		t.Fatal("actual u64-derived size rejected")
	}
	if downloadMetadataMatches(i, obj, int64(obj.CompressedSize)) {
		t.Fatal("u32 sentinel accepted as actual expected length")
	}
}
