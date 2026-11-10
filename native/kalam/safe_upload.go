package main

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ganeshrvel/go-mtpfs/mtp"
	"github.com/ganeshrvel/go-mtpx"
)

// Policy is mandatory. Original-name and Replace commits are intentionally absent.
type SafeUploadInput struct {
	StorageId       uint32  `json:"storageId"`
	Source          string  `json:"source,omitempty"` // Optional display label only; never opened.
	ExpectedSize    uint64  `json:"expectedSize"`
	Destination     string  `json:"destination"`
	RequestedName   string  `json:"requestedName"`
	Policy          string  `json:"policy"`
	ParentId        *uint32 `json:"parentId"` // Required field; explicit null means root.
	parentIdPresent bool
}

func (i *SafeUploadInput) UnmarshalJSON(data []byte) error {
	type alias SafeUploadInput
	var value alias
	if err := json.Unmarshal(data, &value); err != nil {
		return err
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil {
		return err
	}
	_, value.parentIdPresent = fields["parentId"]
	*i = SafeUploadInput(value)
	if !i.parentIdPresent {
		return fmt.Errorf("parentId field is required; use explicit null for root")
	}
	return nil
}

type SafeUploadData struct {
	Committed   bool   `json:"committed"`
	ActualName  string `json:"actualName"`
	ObjectId    uint32 `json:"objectId"`
	Bytes       int64  `json:"bytes"`
	Path        string `json:"path"`
	NeedsReview bool   `json:"needsReview"`
	Phase       string `json:"phase"`
	Operation   string `json:"operation"`
}

// nativeOperationError carries only a fixed code-owned label; no user path,
// object name, IDs or device descriptor is added to this diagnostic label.
// It never logs; the caller may retain phase/operation in a private journal.
type nativeOperationError struct {
	operation string
	err       error
}

func (e nativeOperationError) Error() string { return e.err.Error() }
func (e nativeOperationError) Unwrap() error { return e.err }
func withNativeOperation(operation string, err error) error {
	if err == nil {
		return nil
	}
	return nativeOperationError{operation: operation, err: err}
}
func uploadDiagnostic(phase, operation string, err error) SafeUploadData {
	out := SafeUploadData{Phase: phase, Operation: operation}
	var detail nativeOperationError
	if errors.As(err, &detail) {
		out.Operation = detail.operation
	}
	return out
}

// No delete or rename methods exist on this interface, by construction.
type uploadDevice interface {
	uploadInspectionDevice
	create(storage, parent uint32, info *mtp.ObjectInfo) (uint32, uint32, uint32, error)
	send(io.Reader, int64, mtp.ProgressFunc) error
	inspect(object uint32) (*mtp.ObjectInfo, int64, error)
}

type mtpUploadDevice struct{ dev *mtp.Device }

func validateUploadParentIdentity(storage uint32, originalID *uint32, destination string, resolved *mtpx.FileInfo, bound *mtp.ObjectInfo) (uint32, error) {
	root := originalID == nil || *originalID == 0 || *originalID == 0xffffffff
	if root {
		if destination != "/" || resolved == nil || !resolved.IsDir || !sameParent(resolved.ObjectId, 0xffffffff) {
			return 0, fmt.Errorf("ErrorStaleSession: explicit root destination identity mismatch")
		}
		return mtp.GOH_ROOT_PARENT, nil
	}
	if bound == nil || bound.StorageID != storage || bound.ObjectFormat != mtp.OFC_Association || resolved == nil || !resolved.IsDir || resolved.ObjectId != *originalID {
		return 0, fmt.Errorf("ErrorStaleSession: original upload directory ID no longer matches destination/storage")
	}
	return *originalID, nil
}
func (d mtpUploadDevice) parent(storage uint32, destination string, originalID *uint32) (uint32, error) {
	var bound *mtp.ObjectInfo
	if originalID != nil && *originalID != 0 && *originalID != 0xffffffff {
		bound = &mtp.ObjectInfo{}
		if err := d.dev.GetObjectInfo(*originalID, bound); err != nil {
			return 0, fmt.Errorf("ErrorStaleSession: original upload directory cannot be inspected: %w", withNativeOperation("GetObjectInfo(boundParent)", err))
		}
	}
	resolved, err := mtpx.GetObjectFromPath(d.dev, storage, destination)
	if err != nil {
		return 0, fmt.Errorf("ErrorStaleSession: destination label no longer resolves: %w", withNativeOperation("GetObjectFromPath(destinationLabel)", err))
	}
	return validateUploadParentIdentity(storage, originalID, destination, resolved, bound)
}
func (d mtpUploadDevice) snapshot(storage, parent uint32) (map[uint32]string, error) {
	var handles mtp.Uint32Array
	if err := d.dev.GetObjectHandles(storage, mtp.GOH_ALL_ASSOCS, parent, &handles); err != nil {
		return nil, withNativeOperation("GetObjectHandles(destinationChildren)", err)
	}
	result := make(map[uint32]string, len(handles.Values))
	for _, id := range handles.Values {
		var name mtp.StringValue
		if err := d.dev.GetObjectPropValue(id, mtp.OPC_ObjectFileName, &name); err != nil {
			return nil, withNativeOperation("GetObjectPropValue(destinationChildName)", err)
		}
		result[id] = name.Value
	}
	return result, nil
}
func (d mtpUploadDevice) existingHandles() (map[uint32]bool, error) {
	var handles mtp.Uint32Array
	if err := d.dev.GetObjectHandles(mtp.GOH_ALL_STORAGE, mtp.GOH_ALL_FORMATS, 0, &handles); err != nil {
		return nil, withNativeOperation("GetObjectHandles(allStoragesAllParents)", err)
	}
	result := make(map[uint32]bool, len(handles.Values))
	for _, id := range handles.Values {
		result[id] = true
	}
	return result, nil
}
func (d mtpUploadDevice) create(storage, parent uint32, info *mtp.ObjectInfo) (uint32, uint32, uint32, error) {
	return d.dev.SendObjectInfo(storage, parent, info)
}
func (d mtpUploadDevice) send(r io.Reader, size int64, cb mtp.ProgressFunc) error {
	return d.dev.SendObject(r, size, cb)
}
func (d mtpUploadDevice) inspect(id uint32) (*mtp.ObjectInfo, int64, error) {
	var info mtp.ObjectInfo
	if err := d.dev.GetObjectInfo(id, &info); err != nil {
		return nil, 0, withNativeOperation("GetObjectInfo(transferredObject)", err)
	}
	size, err := mtpx.GetFileSize(d.dev, &info, id, false)
	return &info, size, withNativeOperation("GetObjectPropValue(objectSize)", err)
}

func validateRemotePath(p string) error {
	if !utf8.ValidString(p) || !strings.HasPrefix(p, "/") || strings.ContainsAny(p, "\\\x00") || path.Clean(p) != p {
		return fmt.Errorf("invalid canonical absolute MTP path")
	}
	return nil
}
func validateBasename(name string) error {
	if name == "" || name == "." || name == ".." || !utf8.ValidString(name) || strings.ContainsAny(name, "/\\\x00") {
		return fmt.Errorf("invalid requested basename")
	}
	for _, r := range name {
		if r < 32 || r == 127 {
			return fmt.Errorf("control character in basename")
		}
	}
	return nil
}
func validateUploadInput(i SafeUploadInput) error {
	if i.Policy != "keepBoth" {
		return fmt.Errorf("ErrorUnsafeOperation: only policy keepBoth is supported; Replace is disabled")
	}
	if i.ExpectedSize > uint64(^uint64(0)>>1) {
		return fmt.Errorf("ErrorUnsupportedSize: expectedSize exceeds MaxInt64")
	}
	if err := validateUploadDestinationRef(i.StorageId, i.Destination, i.ParentId, i.parentIdPresent); err != nil {
		return err
	}
	return validateBasename(i.RequestedName)
}

func uniqueBasename(requested string, entropy io.Reader) (string, error) {
	if err := validateBasename(requested); err != nil {
		return "", err
	}
	var random [16]byte
	if _, err := io.ReadFull(entropy, random[:]); err != nil {
		return "", err
	}
	ext := path.Ext(requested)
	// Bound device filename length conservatively without cutting UTF-8 sequences.
	if len(ext) > 24 {
		ext = ""
	}
	stem := strings.TrimSuffix(requested, ext)
	for len(stem) > 120 {
		_, size := utf8.DecodeLastRuneInString(stem)
		stem = stem[:len(stem)-size]
	}
	if stem == "" {
		stem = "file"
	}
	return stem + "-mtp-" + hex.EncodeToString(random[:]) + ext, nil
}
func sameParent(a, b uint32) bool {
	return a == b || ((a == 0 || a == 0xffffffff) && (b == 0 || b == 0xffffffff))
}

type countingReader struct {
	io.Reader
	n int64
}

func (r *countingReader) Read(p []byte) (int, error) {
	n, err := r.Reader.Read(p)
	r.n += int64(n)
	return n, err
}

// safeUpload creates a NEW object with its FINAL KeepBoth name. No path-based
// overwrite helper, rename, directory creation, cleanup, or automatic retry.
func safeUpload(d uploadDevice, i SafeUploadInput, sourceFD int, progress func(*mtpx.ProgressInfo), preprocess func(string, int64)) (out SafeUploadData, err error) {
	out = uploadDiagnostic("validateInput", "validateUploadInput", nil)
	defer func() {
		var detail nativeOperationError
		if err != nil && errors.As(err, &detail) {
			out.Operation = detail.operation
		}
	}()
	if err = validateUploadInput(i); err != nil {
		return
	}
	out.Phase, out.Operation = "bindSourceFd", "DupFstatSeek(sourceStage)"
	file, err := duplicateRegularFD(sourceFD, i.ExpectedSize, false)
	if err != nil {
		return out, err
	}
	defer func() {
		if closeErr := file.Close(); err == nil && closeErr != nil {
			err = closeErr
			out.Phase, out.Operation = "closeSourceFd", "Close(sourceStageDuplicate)"
			out.Committed = false
			out.NeedsReview = out.ObjectId != 0
		}
	}()
	out.Phase, out.Operation = "sourceStat", "Fstat(sourceStage)"
	stat, err := file.Stat()
	if err != nil {
		return out, err
	}
	name := i.RequestedName
	out.Phase, out.Operation = "bindDestination", "ValidateBoundParentAndPath"
	parent, err := d.parent(i.StorageId, i.Destination, i.ParentId)
	if err != nil {
		if strings.HasPrefix(err.Error(), "ErrorStaleSession:") {
			out.NeedsReview = true
		}
		return out, err
	}
	out.Phase, out.Operation = "snapshotDestinationBefore", "Snapshot(destinationChildren)"
	before, err := d.snapshot(i.StorageId, parent)
	if err != nil {
		return out, err
	}
	out.Phase, out.Operation = "snapshotExistingHandles", "GetObjectHandles(allStoragesAllParents)"
	previousIDs, err := d.existingHandles()
	if err != nil {
		return out, err
	}
	out.Phase, out.Operation = "chooseUniqueName", "CryptoRandomBasename"
	for attempt := 0; attempt < 8; attempt++ {
		out.ActualName, err = uniqueBasename(name, rand.Reader)
		if err != nil {
			return out, err
		}
		found := false
		for _, existing := range before {
			if strings.EqualFold(existing, out.ActualName) {
				found = true
				break
			}
		}
		if !found {
			break
		}
		if attempt == 7 {
			return out, fmt.Errorf("could not choose an unused random name")
		}
	}
	out.Path = path.Join(i.Destination, out.ActualName)
	if preprocess != nil {
		preprocess(out.ActualName, stat.Size())
	}
	size := stat.Size()
	compressed := uint32(size)
	if size >= 0xffffffff {
		compressed = 0xffffffff
	}
	obj := mtp.ObjectInfo{StorageID: i.StorageId, ParentObject: parent, Filename: out.ActualName, ObjectFormat: mtp.OFC_Undefined, CompressedSize: compressed, ModificationDate: stat.ModTime()}
	// Any error from create may already have allocated a partial object. Never retry/delete.
	out.NeedsReview = true
	out.Phase, out.Operation = "createObject", "SendObjectInfo"
	storage, returnedParent, id, err := d.create(i.StorageId, parent, &obj)
	out.ObjectId = id
	if err != nil {
		return out, err
	}
	if id == 0 || id == 0xffffffff || storage != i.StorageId || !sameParent(parent, returnedParent) {
		return out, fmt.Errorf("SendObjectInfo returned invalid new-object identity")
	}
	_, existedInParent := before[id]
	if previousIDs[id] || existedInParent {
		return out, fmt.Errorf("SendObjectInfo returned a pre-existing object ID; refusing SendObject")
	}
	start := time.Now()
	p := mtpx.ProgressInfo{FileInfo: &mtpx.FileInfo{ObjectId: id, Name: out.ActualName, FullPath: out.Path, ParentId: parent, Size: size}, StartTime: start, LatestSentTime: start, TotalFiles: 1, ActiveFileSize: &mtpx.TransferSizeInfo{Total: size}, BulkFileSize: &mtpx.TransferSizeInfo{Total: size}, Status: mtpx.InProgress}
	reader := &countingReader{Reader: file}
	out.Phase, out.Operation = "sendObject", "SendObject"
	err = d.send(reader, size, func(sent int64) error {
		if sent < 0 || sent > size || sent < p.ActiveFileSize.Sent {
			return fmt.Errorf("invalid upstream progress byte count")
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
	out.Bytes = reader.n
	if err != nil {
		return out, err
	}
	out.Phase, out.Operation = "verifySourceAfter", "FstatAndConsumedByteCount(sourceStage)"
	if out.Bytes != size {
		return out, fmt.Errorf("SendObject did not consume expected local byte count")
	}
	afterLocal, err := file.Stat()
	if err != nil {
		return out, err
	}
	if afterLocal.Size() != size || !afterLocal.ModTime().Equal(stat.ModTime()) {
		return out, fmt.Errorf("source changed during upload")
	}
	out.Phase, out.Operation = "inspectUploadedObject", "GetObjectInfoAndSize(uploadedObject)"
	actual, remoteSize, err := d.inspect(id)
	if err != nil {
		return out, err
	}
	if actual.Filename != out.ActualName || actual.StorageID != i.StorageId || !sameParent(actual.ParentObject, parent) || actual.ObjectFormat == mtp.OFC_Association || remoteSize != size {
		return out, fmt.Errorf("uploaded object metadata verification mismatch")
	}
	out.Phase, out.Operation = "snapshotDestinationAfter", "Snapshot(destinationChildren)"
	after, err := d.snapshot(i.StorageId, parent)
	if err != nil {
		return out, err
	}
	if after[id] != out.ActualName {
		return out, fmt.Errorf("uploaded object missing from destination")
	}
	for other, existing := range after {
		if other != id && strings.EqualFold(existing, out.ActualName) {
			return out, fmt.Errorf("duplicate final name appeared; review required")
		}
	}
	out.Committed = true
	out.NeedsReview = false
	out.Phase, out.Operation = "complete", "VerifiedKeepBothObject"
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
