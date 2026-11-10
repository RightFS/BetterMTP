package main

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/ganeshrvel/go-mtpfs/mtp"
	"github.com/ganeshrvel/go-mtpx"
)

func TestUploadDirectoryReplacementNeverCreatesOrSends(t *testing.T) {
	d := &fakeUploadDevice{parentReplaced: true}
	out, err := runSafeUpload(t, d, uploadInput(t), nil, nil)
	if err == nil || !strings.Contains(err.Error(), "ErrorStaleSession") || out.Committed || !out.NeedsReview || d.createCalled || d.sent {
		t.Fatalf("out=%+v err=%v create=%v sent=%v", out, err, d.createCalled, d.sent)
	}
}
func TestUploadParentFieldMustBeExplicit(t *testing.T) {
	var i SafeUploadInput
	if err := json.Unmarshal([]byte(`{"storageId":1,"destination":"/","requestedName":"x","policy":"keepBoth","expectedSize":0}`), &i); err == nil {
		t.Fatal("omitted parentId accepted")
	}
	if err := json.Unmarshal([]byte(`{"storageId":1,"destination":"/","parentId":null,"requestedName":"x","policy":"keepBoth","expectedSize":0}`), &i); err != nil {
		t.Fatal(err)
	}
	if err := validateUploadInput(i); err != nil {
		t.Fatal(err)
	}
	if !i.parentIdPresent || i.ParentId != nil {
		t.Fatal("explicit root decoded incorrectly")
	}
	i.Destination = "/not-root"
	if err := validateUploadInput(i); err == nil {
		t.Fatal("root identity with non-root label accepted")
	}
}
func TestUploadParentStorageAndTypeMustMatch(t *testing.T) {
	parent := uint32(42)
	resolved := &mtpx.FileInfo{ObjectId: 42, IsDir: true}
	for _, bound := range []*mtp.ObjectInfo{nil, {StorageID: 2, ObjectFormat: mtp.OFC_Association}, {StorageID: 1, ObjectFormat: mtp.OFC_Undefined}} {
		if _, err := validateUploadParentIdentity(1, &parent, "/existing", resolved, bound); err == nil {
			t.Fatal("invalid bound parent accepted")
		}
	}
}
