package main

import (
	"bytes"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ganeshrvel/go-mtpfs/mtp"
	"github.com/ganeshrvel/go-mtpx"
)

type fakeUploadDevice struct {
	info                                       mtp.ObjectInfo
	old                                        map[uint32]string
	sent                                       bool
	createErr, sendErr                         error
	reused, wrongSize, wrongName, zero         bool
	globalReuse                                bool
	parentReplaced                             bool
	createCalled                               bool
	parentErr, snapshotErr, existingHandlesErr error
	snapshotAfterErr                           error
}

func (d *fakeUploadDevice) existingHandles() (map[uint32]bool, error) {
	if d.existingHandlesErr != nil {
		return nil, d.existingHandlesErr
	}
	out := map[uint32]bool{7: true}
	if d.globalReuse {
		out[99] = true
	}
	return out, nil
}

func (d *fakeUploadDevice) parent(storage uint32, destination string, originalID *uint32) (uint32, error) {
	if d.parentErr != nil {
		return 0, d.parentErr
	}
	resolvedID := uint32(42)
	if d.parentReplaced {
		resolvedID = 43
	}
	if originalID == nil || *originalID == 0 || *originalID == 0xffffffff {
		resolvedID = 0xffffffff
	}
	return validateUploadParentIdentity(storage, originalID, destination, &mtpx.FileInfo{ObjectId: resolvedID, IsDir: true}, &mtp.ObjectInfo{StorageID: storage, ObjectFormat: mtp.OFC_Association})
}
func (d *fakeUploadDevice) snapshot(uint32, uint32) (map[uint32]string, error) {
	if !d.sent && d.snapshotErr != nil {
		return nil, d.snapshotErr
	}
	if d.sent && d.snapshotAfterErr != nil {
		return nil, d.snapshotAfterErr
	}
	out := map[uint32]string{7: "original.txt"}
	if d.old != nil {
		for id, name := range d.old {
			out[id] = name
		}
	}
	if d.sent {
		out[99] = d.info.Filename
	}
	return out, nil
}
func (d *fakeUploadDevice) create(storage, parent uint32, info *mtp.ObjectInfo) (uint32, uint32, uint32, error) {
	d.createCalled = true
	d.info = *info
	id := uint32(99)
	if d.reused {
		id = 7
	}
	if d.zero {
		id = 0
	}
	return storage, parent, id, d.createErr
}
func (d *fakeUploadDevice) send(r io.Reader, size int64, cb mtp.ProgressFunc) error {
	d.sent = true
	if d.sendErr != nil {
		return d.sendErr
	}
	n, err := io.Copy(io.Discard, r)
	if err != nil {
		return err
	}
	return cb(n)
}
func (d *fakeUploadDevice) inspect(uint32) (*mtp.ObjectInfo, int64, error) {
	info := d.info
	if d.wrongName {
		info.Filename = "original.txt"
	}
	size := int64(info.CompressedSize)
	if d.wrongSize {
		size++
	}
	return &info, size, nil
}
func uploadInput(t *testing.T) SafeUploadInput {
	t.Helper()
	source := filepath.Join(t.TempDir(), "original.txt")
	if err := os.WriteFile(source, []byte("test bytes"), 0600); err != nil {
		t.Fatal(err)
	}
	parent := uint32(42)
	return SafeUploadInput{StorageId: 1, Source: source, Destination: "/existing", Policy: "keepBoth", RequestedName: "original.txt", ExpectedSize: 10, ParentId: &parent, parentIdPresent: true}
}
func runSafeUpload(t *testing.T, d uploadDevice, i SafeUploadInput, progress func(*mtpx.ProgressInfo), preprocess func(string, int64)) (SafeUploadData, error) {
	t.Helper()
	file, err := os.Open(i.Source)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	return safeUpload(d, i, int(file.Fd()), progress, preprocess)
}
func TestSafeUploadKeepsOriginalAndVerifiesFinalName(t *testing.T) {
	d := &fakeUploadDevice{}
	i := uploadInput(t)
	var statuses []mtpx.TransferStatus
	out, err := runSafeUpload(t, d, i, func(p *mtpx.ProgressInfo) { statuses = append(statuses, p.Status) }, nil)
	if err != nil {
		t.Fatal(err)
	}
	if !out.Committed || out.NeedsReview || out.ObjectId != 99 || out.Bytes != 10 {
		t.Fatalf("bad result: %+v", out)
	}
	if !strings.HasPrefix(out.ActualName, "original-mtp-") || !strings.HasSuffix(out.ActualName, ".txt") || out.ActualName == "original.txt" {
		t.Fatal(out.ActualName)
	}
	if statuses[len(statuses)-1] != mtpx.Completed {
		t.Fatal(statuses)
	}
}
func TestSafeUploadFailClosed(t *testing.T) {
	cases := []struct {
		name string
		d    fakeUploadDevice
		sent bool
	}{
		{"preexisting ID", fakeUploadDevice{reused: true}, false},
		{"preexisting ID from other storage", fakeUploadDevice{globalReuse: true}, false},
		{"zero ID", fakeUploadDevice{zero: true}, false},
		{"create ambiguous", fakeUploadDevice{createErr: errors.New("USB disconnected")}, false},
		{"send error", fakeUploadDevice{sendErr: errors.New("short transfer")}, true},
		{"metadata size", fakeUploadDevice{wrongSize: true}, true},
		{"metadata filename", fakeUploadDevice{wrongName: true}, true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			out, err := runSafeUpload(t, &c.d, uploadInput(t), nil, nil)
			if err == nil || out.Committed || !out.NeedsReview || c.d.sent != c.sent {
				t.Fatalf("out=%+v err=%v sent=%v", out, err, c.d.sent)
			}
		})
	}
}
func TestSafeUploadRejectsReplaceBeforeDeviceAccess(t *testing.T) {
	i := uploadInput(t)
	i.Policy = "replace"
	out, err := safeUpload(nil, i, -1, nil, nil)
	if err == nil || out.Committed || out.NeedsReview {
		t.Fatalf("%+v %v", out, err)
	}
}
func TestSafeUploadNeverOpensReboundSourceLabel(t *testing.T) {
	i := uploadInput(t)
	file, err := os.Open(i.Source)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	// A completely unrelated label cannot redirect the descriptor-bound read.
	i.Source = "/no-such-path/never-open-me"
	out, err := safeUpload(&fakeUploadDevice{}, i, int(file.Fd()), nil, nil)
	if err != nil || !out.Committed {
		t.Fatalf("%+v %v", out, err)
	}
	if _, err = file.Stat(); err != nil {
		t.Fatal("Rust original fd was closed", err)
	}
}

func TestUniqueBasename(t *testing.T) {
	got, err := uniqueBasename("photo.jpg", bytes.NewReader(make([]byte, 16)))
	if err != nil || got != "photo-mtp-00000000000000000000000000000000.jpg" {
		t.Fatalf("%s %v", got, err)
	}
	if _, err := uniqueBasename("x", bytes.NewReader(nil)); err == nil {
		t.Fatal("entropy failure ignored")
	}
	for _, name := range []string{"", "..", "../x", "x/y", "x\\y", "x\x00y", "x\ny"} {
		if _, err := uniqueBasename(name, bytes.NewReader(make([]byte, 16))); err == nil {
			t.Fatalf("accepted %q", name)
		}
	}
}
func TestRemotePathValidation(t *testing.T) {
	for _, p := range []string{"/", "/DCIM", "/DCIM/a"} {
		if err := validateRemotePath(p); err != nil {
			t.Fatal(err)
		}
	}
	for _, p := range []string{"", "relative", "/../a", "/a/", "/a//b", "/a\\b", "/a\x00b"} {
		if err := validateRemotePath(p); err == nil {
			t.Fatalf("accepted %q", p)
		}
	}
}
func TestDisposeClearsStaleInfoWithNilDevice(t *testing.T) {
	operationMu.Lock()
	defer operationMu.Unlock()
	container.deviceInfo = &mtp.DeviceInfo{}
	if err := _dispose(); err != nil {
		t.Fatal(err)
	}
	if container.dev != nil || container.deviceInfo != nil {
		t.Fatal("stale handles after dispose")
	}
}
