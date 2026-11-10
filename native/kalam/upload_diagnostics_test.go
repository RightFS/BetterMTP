package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"
)

func TestUploadPreflightTimeoutDiagnosticsAndNoCreateInMocks(t *testing.T) {
	timeout := errors.New("LIBUSB_ERROR_TIMEOUT")
	cases := []struct {
		name             string
		device           fakeUploadDevice
		phase, operation string
		review           bool
	}{
		{"bound parent", fakeUploadDevice{parentErr: fmt.Errorf("ErrorStaleSession: bound parent unavailable: %w", withNativeOperation("GetObjectInfo(boundParent)", timeout))}, "bindDestination", "GetObjectInfo(boundParent)", true},
		{"destination path", fakeUploadDevice{parentErr: fmt.Errorf("ErrorStaleSession: path lookup unavailable: %w", withNativeOperation("GetObjectFromPath(destinationLabel)", timeout))}, "bindDestination", "GetObjectFromPath(destinationLabel)", true},
		{"destination handles", fakeUploadDevice{snapshotErr: withNativeOperation("GetObjectHandles(destinationChildren)", timeout)}, "snapshotDestinationBefore", "GetObjectHandles(destinationChildren)", false},
		{"destination names", fakeUploadDevice{snapshotErr: withNativeOperation("GetObjectPropValue(destinationChildName)", timeout)}, "snapshotDestinationBefore", "GetObjectPropValue(destinationChildName)", false},
		{"all device handles", fakeUploadDevice{existingHandlesErr: withNativeOperation("GetObjectHandles(allStoragesAllParents)", timeout)}, "snapshotExistingHandles", "GetObjectHandles(allStoragesAllParents)", false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			input := uploadInput(t)
			out, err := runSafeUpload(t, &c.device, input, nil, nil)
			if !errors.Is(err, timeout) || out.Phase != c.phase || out.Operation != c.operation || out.Committed || out.NeedsReview != c.review {
				t.Fatalf("out=%+v err=%v", out, err)
			}
			if out.ActualName != "" || out.ObjectId != 0 || out.Bytes != 0 || out.Path != "" || c.device.createCalled || c.device.sent {
				t.Fatalf("unexpected mock write: %+v create=%v sent=%v", out, c.device.createCalled, c.device.sent)
			}
			if strings.Contains(out.Phase, input.Source) || strings.Contains(out.Operation, input.Source) || strings.Contains(out.Operation, input.RequestedName) {
				t.Fatal("dynamic label leaked into static diagnostic fields")
			}
			// This assertion proves only the test double was never called. It does not
			// prove the prior real-device UnknownOutcome had no side effects.
		})
	}
}

func TestUploadDiagnosticsPreserveAmbiguousMutationFailures(t *testing.T) {
	timeout := errors.New("LIBUSB_ERROR_TIMEOUT")
	cases := []struct {
		name             string
		device           fakeUploadDevice
		phase, operation string
	}{
		{"create", fakeUploadDevice{createErr: timeout}, "createObject", "SendObjectInfo"},
		{"send", fakeUploadDevice{sendErr: timeout}, "sendObject", "SendObject"},
		{"final snapshot", fakeUploadDevice{snapshotAfterErr: withNativeOperation("GetObjectHandles(destinationChildren)", timeout)}, "snapshotDestinationAfter", "GetObjectHandles(destinationChildren)"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			out, err := runSafeUpload(t, &c.device, uploadInput(t), nil, nil)
			if !errors.Is(err, timeout) || out.Committed || !out.NeedsReview || out.Phase != c.phase || out.Operation != c.operation || !c.device.createCalled {
				t.Fatalf("out=%+v err=%v create=%v", out, err, c.device.createCalled)
			}
			if out.ActualName == "" || out.Path == "" {
				t.Fatal("mutation review identity was discarded")
			}
		})
	}
}

func TestStaticDiagnosticEnvelopeAndErrorChain(t *testing.T) {
	failure := errors.New("original transport error")
	wrapped := fmt.Errorf("outer context: %w", withNativeOperation("GetObjectHandles(allStoragesAllParents)", failure))
	out := uploadDiagnostic("snapshotExistingHandles", "fallback", wrapped)
	if !errors.Is(wrapped, failure) || out.Operation != "GetObjectHandles(allStoragesAllParents)" {
		t.Fatal(out, wrapped)
	}
	data, err := json.Marshal(out)
	if err != nil || !strings.Contains(string(data), `"phase":"snapshotExistingHandles"`) || !strings.Contains(string(data), `"operation":"GetObjectHandles(allStoragesAllParents)"`) {
		t.Fatalf("%s %v", data, err)
	}
	input := SafeUploadInput{Policy: "replace"}
	invalid, err := safeUpload(nil, input, -1, nil, nil)
	if err == nil || invalid.Phase != "validateInput" || invalid.Operation != "validateUploadInput" {
		t.Fatal(invalid, err)
	}
}
