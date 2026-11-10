package main

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/ganeshrvel/go-mtpfs/mtp"
)

type fakeDownloadDevice struct {
	content   []byte
	size      int64
	object    mtp.ObjectInfo
	getCalled bool
}

func (d *fakeDownloadDevice) inspect(uint32) (*mtp.ObjectInfo, int64, error) {
	obj := d.object
	return &obj, d.size, nil
}
func (d *fakeDownloadDevice) get(_ uint32, w io.Writer, cb mtp.ProgressFunc) error {
	d.getCalled = true
	n, err := io.Copy(w, bytes.NewReader(d.content))
	if err != nil {
		return err
	}
	return cb(n)
}
func downloadFixture(t *testing.T) (*fakeDownloadDevice, SafeDownloadInput, *os.File) {
	t.Helper()
	f, err := os.OpenFile(filepath.Join(t.TempDir(), "stage"), os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = f.Close() })
	d := &fakeDownloadDevice{content: []byte("hello"), size: 5, object: mtp.ObjectInfo{StorageID: 1, ParentObject: 42, Filename: "hello.txt", ObjectFormat: mtp.OFC_Undefined, CompressedSize: 5}}
	parent := uint32(42)
	return d, SafeDownloadInput{StorageId: 1, ObjectId: 99, ExpectedSize: 5, ParentId: &parent}, f
}
func TestSafeDownloadWritesExactStageFDAndKeepsOriginalOpen(t *testing.T) {
	d, i, f := downloadFixture(t)
	out, err := safeDownload(d, i, int(f.Fd()), nil)
	if err != nil || !out.Committed || out.ObjectId != 99 || out.Bytes != 5 {
		t.Fatalf("%+v %v", out, err)
	}
	if _, err = f.Seek(0, io.SeekStart); err != nil {
		t.Fatal("Rust fd closed", err)
	}
	got, err := io.ReadAll(f)
	if err != nil || string(got) != "hello" {
		t.Fatalf("%q %v", got, err)
	}
}
func TestSafeDownloadDoesNotFollowReboundStagePath(t *testing.T) {
	d, i, f := downloadFixture(t)
	stagePath := f.Name()
	saved := stagePath + ".bound"
	// Test fixture paths are known exact paths in t.TempDir; rename binds the same inode.
	if err := os.Rename(stagePath, saved); err != nil {
		t.Fatal(err)
	}
	victim := stagePath + ".victim"
	if err := os.WriteFile(victim, []byte("DO NOT TOUCH"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(victim, stagePath); err != nil {
		t.Fatal(err)
	}
	out, err := safeDownload(d, i, int(f.Fd()), nil)
	if err != nil || !out.Committed {
		t.Fatalf("%+v %v", out, err)
	}
	unchanged, err := os.ReadFile(victim)
	if err != nil || string(unchanged) != "DO NOT TOUCH" {
		t.Fatalf("victim changed: %q %v", unchanged, err)
	}
	actual, err := os.ReadFile(saved)
	if err != nil || string(actual) != "hello" {
		t.Fatalf("bound inode content %q %v", actual, err)
	}
}
func TestSafeDownloadRejectsMetadataBeforeWriting(t *testing.T) {
	d, i, f := downloadFixture(t)
	i.StorageId = 2
	out, err := safeDownload(d, i, int(f.Fd()), nil)
	if err == nil || out.Committed || d.getCalled {
		t.Fatalf("%+v %v", out, err)
	}
	stat, _ := f.Stat()
	if stat.Size() != 0 {
		t.Fatal("wrote invalid object")
	}
}
func TestSafeDownloadRejectsNonemptyStage(t *testing.T) {
	d, i, f := downloadFixture(t)
	_, _ = f.Write([]byte("old"))
	out, err := safeDownload(d, i, int(f.Fd()), nil)
	if err == nil || out.Committed || d.getCalled {
		t.Fatalf("%+v %v", out, err)
	}
}
func TestSafeDownloadRejectsExcessAndShortBytes(t *testing.T) {
	for _, content := range []string{"hello-extra", "hi"} {
		t.Run(content, func(t *testing.T) {
			d, i, f := downloadFixture(t)
			d.content = []byte(content)
			out, err := safeDownload(d, i, int(f.Fd()), nil)
			if err == nil || out.Committed {
				t.Fatalf("%+v %v", out, err)
			}
			stat, _ := f.Stat()
			if stat.Size() > 5 {
				t.Fatal("excess bytes written")
			}
		})
	}
}
func TestDuplicateRejectsDirectory(t *testing.T) {
	f, err := os.Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if file, err := duplicateRegularFD(int(f.Fd()), 0, true); err == nil {
		file.Close()
		t.Fatal("directory accepted")
	}
}
