package main

import (
	"fmt"
	"io"
	"os"
	"syscall"
	"time"

	"github.com/ganeshrvel/go-mtpfs/mtp"
	"github.com/ganeshrvel/go-mtpx"
)

// Duplicate immediately: no ambient pathname is ever read or written here.
// dup shares the file offset; Rust must keep the original FD alive and exclusive
// throughout the synchronous call. Closing this file never closes Rust's FD.
func duplicateRegularFD(fd int, expected uint64, empty bool) (*os.File, error) {
	if fd < 0 || expected > uint64(^uint64(0)>>1) {
		return nil, fmt.Errorf("invalid descriptor or expectedSize")
	}
	owned, err := syscall.Dup(fd)
	if err != nil {
		return nil, err
	}
	syscall.CloseOnExec(owned)
	file := os.NewFile(uintptr(owned), "rust-owned-stage-duplicate")
	fail := func(err error) (*os.File, error) { _ = file.Close(); return nil, err }
	stat, err := file.Stat()
	if err != nil {
		return fail(err)
	}
	if !stat.Mode().IsRegular() {
		return fail(fmt.Errorf("stage descriptor must refer to a regular file"))
	}
	if (empty && stat.Size() != 0) || (!empty && uint64(stat.Size()) != expected) {
		return fail(fmt.Errorf("stage descriptor length mismatch"))
	}
	if empty {
		flags, _, errno := syscall.Syscall(syscall.SYS_FCNTL, uintptr(owned), uintptr(syscall.F_GETFL), 0)
		if errno != 0 {
			return fail(errno)
		}
		if flags&syscall.O_APPEND != 0 {
			return fail(fmt.Errorf("append stage descriptor is not allowed"))
		}
	}
	if _, err := file.Seek(0, io.SeekStart); err != nil {
		return fail(err)
	}
	return file, nil
}

type SafeDownloadInput struct {
	StorageId    uint32  `json:"storageId"`
	ObjectId     uint32  `json:"objectId"`
	ExpectedSize uint64  `json:"expectedSize"`
	ParentId     *uint32 `json:"parentId,omitempty"`
}
type SafeDownloadData struct {
	Committed bool   `json:"committed"` // Stage complete, NOT final local publication.
	ObjectId  uint32 `json:"objectId"`
	Bytes     int64  `json:"bytes"`
}
type downloadDevice interface {
	inspect(uint32) (*mtp.ObjectInfo, int64, error)
	get(uint32, io.Writer, mtp.ProgressFunc) error
}

func (d mtpUploadDevice) get(id uint32, w io.Writer, cb mtp.ProgressFunc) error {
	return d.dev.GetObject(id, w, cb)
}

type boundedWriter struct {
	writer   io.Writer
	n, limit int64
}

func (w *boundedWriter) Write(p []byte) (int, error) {
	if int64(len(p)) > w.limit-w.n {
		return 0, fmt.Errorf("remote object exceeds expectedSize; refusing excess bytes")
	}
	n, err := w.writer.Write(p)
	w.n += int64(n)
	if err == nil && n != len(p) {
		err = io.ErrShortWrite
	}
	return n, err
}
func downloadMetadataMatches(i SafeDownloadInput, obj *mtp.ObjectInfo, size int64) bool {
	return obj != nil && obj.StorageID == i.StorageId && obj.ObjectFormat != mtp.OFC_Association && size >= 0 && uint64(size) == i.ExpectedSize && (i.ParentId == nil || sameParent(*i.ParentId, obj.ParentObject))
}
func safeDownload(d downloadDevice, i SafeDownloadInput, outputFD int, progress func(*mtpx.ProgressInfo)) (out SafeDownloadData, err error) {
	out.ObjectId = i.ObjectId
	if i.ExpectedSize > uint64(^uint64(0)>>1) {
		return out, fmt.Errorf("ErrorUnsupportedSize: expectedSize exceeds MaxInt64")
	}
	if i.StorageId == 0 || i.ObjectId == 0 || i.ObjectId == 0xffffffff {
		return out, fmt.Errorf("invalid storageId/objectId")
	}
	file, err := duplicateRegularFD(outputFD, i.ExpectedSize, true)
	if err != nil {
		return out, err
	}
	defer func() {
		if closeErr := file.Close(); err == nil && closeErr != nil {
			err = closeErr
		}
		if err != nil {
			out.Committed = false
		}
	}()
	before, size, err := d.inspect(i.ObjectId)
	if err != nil {
		return out, err
	}
	if !downloadMetadataMatches(i, before, size) {
		return out, fmt.Errorf("source object metadata mismatch before download")
	}
	start := time.Now()
	p := mtpx.ProgressInfo{FileInfo: &mtpx.FileInfo{ObjectId: i.ObjectId, Name: before.Filename, ParentId: before.ParentObject, Size: size}, StartTime: start, LatestSentTime: start, TotalFiles: 1, ActiveFileSize: &mtpx.TransferSizeInfo{Total: size}, BulkFileSize: &mtpx.TransferSizeInfo{Total: size}, Status: mtpx.InProgress}
	writer := &boundedWriter{writer: file, limit: size}
	err = d.get(i.ObjectId, writer, func(sent int64) error {
		if sent < 0 || sent > size || sent < p.ActiveFileSize.Sent {
			return fmt.Errorf("invalid upstream download progress count")
		}
		p.ActiveFileSize.Sent = sent
		p.BulkFileSize.Sent = sent
		if size > 0 {
			p.ActiveFileSize.Progress = float32(sent) * 100 / float32(size)
			p.BulkFileSize.Progress = p.ActiveFileSize.Progress
		}
		if progress != nil {
			progress(&p)
		}
		return nil
	})
	out.Bytes = writer.n
	if err != nil {
		return out, err
	}
	stat, err := file.Stat()
	if err != nil {
		return out, err
	}
	if writer.n != size || stat.Size() != size {
		return out, fmt.Errorf("download byte count or stage length mismatch")
	}
	after, afterSize, err := d.inspect(i.ObjectId)
	if err != nil {
		return out, err
	}
	if !downloadMetadataMatches(i, after, afterSize) || after.Filename != before.Filename || after.ParentObject != before.ParentObject || !after.ModificationDate.Equal(before.ModificationDate) {
		return out, fmt.Errorf("source object changed during download")
	}
	if err = file.Sync(); err != nil {
		return out, err
	}
	out.Committed = true
	p.Status = mtpx.Completed
	p.FilesSent = 1
	p.FilesSentProgress = 100
	p.ActiveFileSize.Sent = size
	p.ActiveFileSize.Progress = 100
	p.BulkFileSize.Sent = size
	p.BulkFileSize.Progress = 100
	if progress != nil {
		progress(&p)
	}
	return out, nil
}
