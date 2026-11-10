// Derived from OpenMTP Kalam; see LICENSE and SOURCE.md.
package main

/*
#include <stdint.h>
typedef void (*on_cb_result_t)(char*);
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"kalam/send_to_js"
	"sync"

	"github.com/ganeshrvel/go-mtpx"
)

var container deviceContainer
var operationMu sync.Mutex

// SafetyVersion is a hardening ABI marker, not device certification.
//
//export SafetyVersion
func SafetyVersion() C.uint32_t { return 2 }

// All callbacks run while operationMu is held. They must never reenter Kalam.
//
//export Initialize
func Initialize(cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	// Dispose an old session before replacing it and clear partially initialized state.
	_ = _dispose()
	ok := false
	defer func() {
		if !ok {
			_ = _dispose()
		}
	}()
	if _, err := _initialize(mtpx.Init{DebugMode: false}); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	info, err := _fetchDeviceInfo()
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	usb, err := container.dev.GetUsbInfo()
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	ok = true
	send_to_js.SendInitialize(done, info, usb)
}

//export FetchDeviceInfo
func FetchDeviceInfo(cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	info, err := _fetchDeviceInfo()
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	usb, err := container.dev.GetUsbInfo()
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendDeviceInfo(done, info, usb)
}

//export FetchStorages
func FetchStorages(cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	storages, err := _fetchStorages()
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendStorages(done, storages)
}

//export MakeDirectory
func MakeDirectory(input *C.char, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i MakeDirectoryInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := validateRemotePath(i.FullPath); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := _makeDirectory(i.StorageId, i.FullPath); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	// Boolean legacy result conveys no ownership of the directory.
	send_to_js.SendMakeDirectory(done)
}

//export FileExists
func FileExists(input *C.char, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i FileExistsInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	props := make([]mtpx.FileProp, 0, len(i.Files))
	for _, p := range i.Files {
		if err := validateRemotePath(p); err != nil {
			send_to_js.SendError(done, err)
			return
		}
		props = append(props, mtpx.FileProp{FullPath: p})
	}
	files, err := _fileExists(i.StorageId, props)
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendFileExists(done, files, i.Files)
}

// Disabled until explicit object-ID ownership/confirmation and no-clobber semantics exist.
//
//export DeleteFile
func DeleteFile(input *C.char, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	send_to_js.SendError((*send_to_js.SendCbResult)(cb), fmt.Errorf("ErrorUnsafeOperation: DeleteFile is disabled in SafetyVersion 2 MVP"))
}

//export RenameFile
func RenameFile(input *C.char, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	send_to_js.SendError((*send_to_js.SendCbResult)(cb), fmt.Errorf("ErrorUnsafeOperation: RenameFile has no proven no-clobber commit; disabled"))
}

//export Walk
func Walk(input *C.char, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i WalkInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := validateRemotePath(i.FullPath); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	files, err := _walk(i.StorageId, i.FullPath, i.Recursive, i.SkipDisallowedFiles, i.SkipHiddenFiles)
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendWalk(done, files)
}

// The old bulk ABI is retained fail-closed, never routed to overwriteExisting=true.
//
//export UploadFiles
func UploadFiles(input *C.char, pre, progress, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	send_to_js.SendError((*send_to_js.SendCbResult)(cb), fmt.Errorf("ErrorUnsafeOperation: legacy UploadFiles is disabled; use SafeUploadSingle keepBoth"))
}

//export SafeUploadSingle
func SafeUploadSingle(input *C.char, sourceFD C.int, pre, progress, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i SafeUploadInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := validateUploadInput(i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := verifyMtpSession(verifyMtpSessionMode{}); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	result, err := safeUpload(mtpUploadDevice{container.dev}, i, int(sourceFD), func(p *mtpx.ProgressInfo) {
		send_to_js.SendTransferFilesProgress((*send_to_js.SendCbResult)(progress), p)
	}, func(name string, size int64) {
		send_to_js.SendValue((*send_to_js.SendCbResult)(pre), send_to_js.ErrorResult{Data: send_to_js.TransferPreprocessData{FullPath: i.Source, Name: name, Size: size}})
	})
	envelope := send_to_js.ErrorResult{Data: result}
	if err != nil {
		envelope.Error = err.Error()
		envelope.ErrorType = "ErrorGeneral"
		if result.NeedsReview {
			envelope.ErrorType = "ErrorNeedsReview"
		}
	}
	send_to_js.SendValue(done, envelope)
}

// SafeDownloadSingle completes only a descriptor-bound stage, not publication.
//
//export SafeDownloadSingle
func SafeDownloadSingle(input *C.char, outputFD C.int, progress, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i SafeDownloadInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	if err := verifyMtpSession(verifyMtpSessionMode{}); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	result, err := safeDownload(mtpUploadDevice{container.dev}, i, int(outputFD), func(p *mtpx.ProgressInfo) {
		send_to_js.SendTransferFilesProgress((*send_to_js.SendCbResult)(progress), p)
	})
	envelope := send_to_js.ErrorResult{Data: result}
	if err != nil {
		envelope.ErrorType = "ErrorGeneral"
		envelope.Error = err.Error()
	}
	send_to_js.SendValue(done, envelope)
}

// Compatibility only: ambient-path download remains unsafe against rebinding.
// Rust MUST NOT call it; use SafeDownloadSingle with an owned staging FD.
//
//export DownloadFiles
func DownloadFiles(input *C.char, pre, progress, cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	var i DownloadFilesInput
	if err := json.Unmarshal([]byte(C.GoString(input)), &i); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	for _, p := range i.Sources {
		if err := validateRemotePath(p); err != nil {
			send_to_js.SendError(done, err)
			return
		}
	}
	err := _downloadFiles(i.StorageId, i.Sources, i.Destination, i.PreprocessFiles,
		func(fi *mtpx.FileInfo, err error) error {
			if err != nil {
				return err
			}
			send_to_js.SendDownloadFilesPreprocess((*send_to_js.SendCbResult)(pre), fi)
			return nil
		}, func(p *mtpx.ProgressInfo, err error) error {
			if err != nil {
				return err
			}
			send_to_js.SendTransferFilesProgress((*send_to_js.SendCbResult)(progress), p)
			return nil
		})
	if err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendTransferFilesDone(done)
}

//export Dispose
func Dispose(cb *C.on_cb_result_t) {
	operationMu.Lock()
	defer operationMu.Unlock()
	done := (*send_to_js.SendCbResult)(cb)
	if err := _dispose(); err != nil {
		send_to_js.SendError(done, err)
		return
	}
	send_to_js.SendDispose(done)
}

func main() {}
