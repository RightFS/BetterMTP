package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/ganeshrvel/go-mtpfs/mtp"
	"kalam/send_to_js"
)

// Must run inside operationMu. This changes only local Go debug flags, never
// device state. The caller restores them before releasing the operation lock.
func suppressInspectionDebug(dev *mtp.Device) func() {
	if dev == nil {
		return func() {}
	}
	mtpDebug, dataDebug, usbDebug := dev.MTPDebug, dev.DataDebug, dev.USBDebug
	dev.MTPDebug, dev.DataDebug, dev.USBDebug = false, false, false
	return func() { dev.MTPDebug, dev.DataDebug, dev.USBDebug = mtpDebug, dataDebug, usbDebug }
}

// The inspection core receives only these read methods. It cannot invoke any
// upload/create/send/delete/rename/mkdir operation or generate a new basename.
type uploadInspectionDevice interface {
	parent(storage uint32, destination string, originalID *uint32) (uint32, error)
	snapshot(storage, parent uint32) (map[uint32]string, error)
	existingHandles() (map[uint32]bool, error)
}

type InspectUploadDestinationInput struct {
	StorageId       uint32  `json:"storageId"`
	ParentId        *uint32 `json:"parentId"`
	Destination     string  `json:"destination"`
	parentIdPresent bool
}

func (i *InspectUploadDestinationInput) UnmarshalJSON(data []byte) error {
	type alias InspectUploadDestinationInput
	var value alias
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&value); err != nil {
		return err
	}
	if err := decoder.Decode(new(interface{})); err != io.EOF {
		return fmt.Errorf("inspection input must be one JSON object")
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil {
		return err
	}
	_, value.parentIdPresent = fields["parentId"]
	*i = InspectUploadDestinationInput(value)
	if !i.parentIdPresent {
		return fmt.Errorf("parentId field is required; use explicit null for root")
	}
	return nil
}
func validateUploadDestinationRef(storage uint32, destination string, parentID *uint32, present bool) error {
	if storage == 0 {
		return fmt.Errorf("invalid storageId")
	}
	if !present {
		return fmt.Errorf("parentId field is required; explicit null means root")
	}
	if (parentID == nil || *parentID == 0 || *parentID == 0xffffffff) && destination != "/" {
		return fmt.Errorf("ErrorStaleSession: root parentId requires destination / ")
	}
	return validateRemotePath(destination)
}

type InspectUploadDestinationData struct {
	NoWrites              bool   `json:"noWrites"`
	Complete              bool   `json:"complete"`
	Phase                 string `json:"phase"`
	Operation             string `json:"operation"`
	DestinationChildCount uint64 `json:"destinationChildCount"`
	ExistingHandleCount   uint64 `json:"existingHandleCount"`
}

func inspectionDiagnostic(phase, operation string, err error) InspectUploadDestinationData {
	out := InspectUploadDestinationData{NoWrites: true, Phase: phase, Operation: operation}
	var detail nativeOperationError
	if errors.As(err, &detail) {
		out.Operation = detail.operation
	}
	return out
}

// noWrites refers to object/content/directory mutation, not to USB OUT packets
// carrying read-only MTP command headers. No source FD or filename is accepted.
// Names and global IDs are read internally using the existing SDK contract;
// only counts of completed snapshots are returned. No content is read.
func inspectUploadDestination(d uploadInspectionDevice, i InspectUploadDestinationInput) (out InspectUploadDestinationData, err error) {
	out = inspectionDiagnostic("validateInput", "validateUploadDestinationRef", nil)
	defer func() {
		var detail nativeOperationError
		if err != nil && errors.As(err, &detail) {
			out.Operation = detail.operation
		}
	}()
	if err = validateUploadDestinationRef(i.StorageId, i.Destination, i.ParentId, i.parentIdPresent); err != nil {
		return
	}
	out.Phase, out.Operation = "bindDestination", "ValidateBoundParentAndPath"
	parent, err := d.parent(i.StorageId, i.Destination, i.ParentId)
	if err != nil {
		return out, err
	}
	out.Phase, out.Operation = "snapshotDestinationBefore", "Snapshot(destinationChildren)"
	children, err := d.snapshot(i.StorageId, parent)
	if err != nil {
		return out, err
	}
	out.DestinationChildCount = uint64(len(children))
	out.Phase, out.Operation = "snapshotExistingHandles", "GetObjectHandles(allStoragesAllParents)"
	existing, err := d.existingHandles()
	if err != nil {
		return out, err
	}
	out.ExistingHandleCount = uint64(len(existing))
	out.Complete = true
	out.Phase, out.Operation = "complete", "InspectedUploadDestinationReads"
	return out, nil
}

// Unlike upload's private error envelope, this count-only inspection API never
// returns raw SDK errors that can embed paths/filenames. Transport codes use a
// fixed allowlist; all other error messages are static and privacy-safe.
func inspectionEnvelope(out InspectUploadDestinationData, err error) send_to_js.ErrorResult {
	result := send_to_js.ErrorResult{Data: out}
	if err == nil {
		return result
	}
	result.ErrorType = "ErrorGeneral"
	result.Error = "Read-only upload destination inspection failed"
	raw := err.Error()
	for _, code := range []string{"LIBUSB_ERROR_IO", "LIBUSB_ERROR_INVALID_PARAM", "LIBUSB_ERROR_ACCESS", "LIBUSB_ERROR_NO_DEVICE", "LIBUSB_ERROR_NOT_FOUND", "LIBUSB_ERROR_BUSY", "LIBUSB_ERROR_TIMEOUT", "LIBUSB_ERROR_OVERFLOW", "LIBUSB_ERROR_PIPE", "LIBUSB_ERROR_INTERRUPTED", "LIBUSB_ERROR_NO_MEM", "LIBUSB_ERROR_NOT_SUPPORTED", "LIBUSB_ERROR_OTHER"} {
		if strings.Contains(raw, code) {
			result.Error = code
			return result
		}
	}
	if strings.HasPrefix(raw, "ErrorStaleSession:") {
		result.ErrorType = "ErrorStaleSession"
		result.Error = "Upload destination identity is stale"
	}
	return result
}
