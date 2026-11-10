package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io"
	"reflect"
	"strings"
	"testing"

	"github.com/ganeshrvel/go-mtpfs/mtp"
)

type poisonInspectionDevice struct {
	fakeUploadDevice
	readCalls  []string
	global     map[uint32]bool
	writeCalls int
}

func (d *poisonInspectionDevice) parent(storage uint32, destination string, id *uint32) (uint32, error) {
	d.readCalls = append(d.readCalls, "parent")
	return d.fakeUploadDevice.parent(storage, destination, id)
}
func (d *poisonInspectionDevice) snapshot(storage, parent uint32) (map[uint32]string, error) {
	d.readCalls = append(d.readCalls, "children")
	return d.fakeUploadDevice.snapshot(storage, parent)
}
func (d *poisonInspectionDevice) existingHandles() (map[uint32]bool, error) {
	d.readCalls = append(d.readCalls, "global")
	if d.existingHandlesErr != nil {
		return nil, d.existingHandlesErr
	}
	if d.global != nil {
		return d.global, nil
	}
	return d.fakeUploadDevice.existingHandles()
}
func (d *poisonInspectionDevice) create(uint32, uint32, *mtp.ObjectInfo) (uint32, uint32, uint32, error) {
	d.writeCalls++
	panic("poison create called")
}
func (d *poisonInspectionDevice) send(io.Reader, int64, mtp.ProgressFunc) error {
	d.writeCalls++
	panic("poison send called")
}
func (d *poisonInspectionDevice) DeleteObject(uint32) error {
	d.writeCalls++
	panic("poison delete called")
}
func (d *poisonInspectionDevice) RenameFile(uint32, string) error {
	d.writeCalls++
	panic("poison rename called")
}
func (d *poisonInspectionDevice) MakeDirectory(string) error {
	d.writeCalls++
	panic("poison mkdir called")
}
func (d *poisonInspectionDevice) GetObject(uint32, io.Writer, mtp.ProgressFunc) error {
	panic("file body reads are forbidden")
}
func inspectInput() InspectUploadDestinationInput {
	parent := uint32(42)
	return InspectUploadDestinationInput{StorageId: 1, ParentId: &parent, Destination: "/existing", parentIdPresent: true}
}
func TestInspectDestinationReadChainOnlyReturnsCounts(t *testing.T) {
	d := &poisonInspectionDevice{fakeUploadDevice: fakeUploadDevice{old: map[uint32]string{100: "PRIVATE-NAME-A", 200: "PRIVATE-NAME-B"}}, global: map[uint32]bool{7: true, 100: true, 200: true, 900: true}}
	out, err := inspectUploadDestination(d, inspectInput())
	if err != nil || !out.NoWrites || !out.Complete || out.DestinationChildCount != 3 || out.ExistingHandleCount != 4 || out.Phase != "complete" || out.Operation != "InspectedUploadDestinationReads" {
		t.Fatalf("%+v %v", out, err)
	}
	if !reflect.DeepEqual(d.readCalls, []string{"parent", "children", "global"}) || d.writeCalls != 0 {
		t.Fatal(d.readCalls, d.writeCalls)
	}
	data, err := json.Marshal(inspectionEnvelope(out, nil))
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{"PRIVATE-NAME", "original.txt", "/existing", "objectId", "storageId", "actualName", "\"900\"", "path"} {
		if strings.Contains(string(data), secret) {
			t.Fatalf("inspection leaked %q: %s", secret, data)
		}
	}
	if len(data) > 1024 {
		t.Fatal("count-only result unexpectedly large")
	}
}
func TestInspectionDebugSuppressionRestoresFlagsWithoutDeviceAccess(t *testing.T) {
	dev := &mtp.Device{MTPDebug: true, DataDebug: false, USBDebug: true}
	restore := suppressInspectionDebug(dev)
	if dev.MTPDebug || dev.DataDebug || dev.USBDebug {
		t.Fatal("SDK metadata debugging was not suppressed")
	}
	restore()
	if !dev.MTPDebug || dev.DataDebug || !dev.USBDebug {
		t.Fatal("SDK debug flags were not restored")
	}
	suppressInspectionDebug(nil)()
}

func TestInspectReadFailuresNeverCallPoisonWrites(t *testing.T) {
	timeout := errors.New("LIBUSB_ERROR_TIMEOUT")
	cases := []struct {
		name       string
		d          fakeUploadDevice
		phase, op  string
		calls      []string
		childCount uint64
	}{
		{"bound parent", fakeUploadDevice{parentErr: withNativeOperation("GetObjectInfo(boundParent)", timeout)}, "bindDestination", "GetObjectInfo(boundParent)", []string{"parent"}, 0},
		{"path", fakeUploadDevice{parentErr: withNativeOperation("GetObjectFromPath(destinationLabel)", timeout)}, "bindDestination", "GetObjectFromPath(destinationLabel)", []string{"parent"}, 0},
		{"parent handles", fakeUploadDevice{snapshotErr: withNativeOperation("GetObjectHandles(destinationChildren)", timeout)}, "snapshotDestinationBefore", "GetObjectHandles(destinationChildren)", []string{"parent", "children"}, 0},
		{"parent name", fakeUploadDevice{snapshotErr: withNativeOperation("GetObjectPropValue(destinationChildName)", timeout)}, "snapshotDestinationBefore", "GetObjectPropValue(destinationChildName)", []string{"parent", "children"}, 0},
		{"all handles", fakeUploadDevice{existingHandlesErr: withNativeOperation("GetObjectHandles(allStoragesAllParents)", timeout)}, "snapshotExistingHandles", "GetObjectHandles(allStoragesAllParents)", []string{"parent", "children", "global"}, 1},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			d := &poisonInspectionDevice{fakeUploadDevice: c.d}
			out, err := inspectUploadDestination(d, inspectInput())
			if !errors.Is(err, timeout) || !out.NoWrites || out.Complete || out.Phase != c.phase || out.Operation != c.op || out.DestinationChildCount != c.childCount || out.ExistingHandleCount != 0 {
				t.Fatalf("%+v %v", out, err)
			}
			if !reflect.DeepEqual(d.readCalls, c.calls) || d.writeCalls != 0 || d.createCalled || d.sent {
				t.Fatal(d.readCalls, d.writeCalls, d.createCalled, d.sent)
			}
		})
	}
}
func TestInspectInputExplicitRootAndNoUploadFields(t *testing.T) {
	var i InspectUploadDestinationInput
	for _, raw := range []string{`{"storageId":1,"destination":"/"}`, `{"storageId":1,"parentId":null,"destination":"/","policy":"keepBoth"}`, `{"storageId":1,"parentId":null,"destination":"/","sourceFd":5}`} {
		if err := json.Unmarshal([]byte(raw), &i); err == nil {
			t.Fatalf("accepted %s", raw)
		}
	}
	if err := json.Unmarshal([]byte(`{"storageId":1,"parentId":null,"destination":"/"}`), &i); err != nil {
		t.Fatal(err)
	}
	d := &poisonInspectionDevice{}
	out, err := inspectUploadDestination(d, i)
	if err != nil || !out.Complete || !out.NoWrites || d.writeCalls != 0 {
		t.Fatal(out, err)
	}
	i.Destination = "/not-root"
	d = &poisonInspectionDevice{}
	out, err = inspectUploadDestination(d, i)
	if err == nil || !out.NoWrites || out.Complete || len(d.readCalls) != 0 {
		t.Fatal(out, err, d.readCalls)
	}
}
func TestInspectErrorEnvelopeNeverReturnsRawPrivateNames(t *testing.T) {
	out := inspectionDiagnostic("bindDestination", "GetObjectFromPath(destinationLabel)", nil)
	for _, failure := range []error{fmt.Errorf("ErrorStaleSession: path /PRIVATE-NAME unavailable"), fmt.Errorf("PRIVATE-NAME: LIBUSB_ERROR_TIMEOUT"), fmt.Errorf("PRIVATE-NAME and device descriptor")} {
		result := inspectionEnvelope(out, failure)
		data, err := json.Marshal(result)
		if err != nil || strings.Contains(string(data), "PRIVATE-NAME") || !out.NoWrites {
			t.Fatalf("%s %v", data, err)
		}
	}
}
func TestInspectCoreInterfaceAndSourceCannotCallMutatingOperations(t *testing.T) {
	iface := reflect.TypeOf((*uploadInspectionDevice)(nil)).Elem()
	expected := map[string]bool{"parent": true, "snapshot": true, "existingHandles": true}
	if iface.NumMethod() != len(expected) {
		t.Fatal("read-only interface changed", iface.NumMethod())
	}
	for i := 0; i < iface.NumMethod(); i++ {
		if !expected[iface.Method(i).Name] {
			t.Fatal("unsafe inspection interface method", iface.Method(i).Name)
		}
	}
	source, err := parser.ParseFile(token.NewFileSet(), "inspect_upload_destination.go", nil, 0)
	if err != nil {
		t.Fatal(err)
	}
	forbidden := map[string]bool{"safeUpload": true, "uniqueBasename": true, "create": true, "send": true, "DeleteObject": true, "SendObject": true, "SendObjectInfo": true, "RenameFile": true, "MakeDirectory": true, "UploadFiles": true, "DownloadFiles": true, "GetObject": true}
	ast.Inspect(source, func(node ast.Node) bool {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return true
		}
		name := ""
		switch fn := call.Fun.(type) {
		case *ast.Ident:
			name = fn.Name
		case *ast.SelectorExpr:
			name = fn.Sel.Name
		}
		if forbidden[name] {
			t.Fatalf("forbidden call in read-only inspection source: %s", name)
		}
		return true
	})
	for _, path := range []string{"safe_upload.go", "kalam.go"} {
		file, err := parser.ParseFile(token.NewFileSet(), path, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		for _, decl := range file.Decls {
			fn, ok := decl.(*ast.FuncDecl)
			if !ok {
				continue
			}
			readAdapter := path == "safe_upload.go" && (fn.Name.Name == "parent" || fn.Name.Name == "snapshot" || fn.Name.Name == "existingHandles" || fn.Name.Name == "validateUploadParentIdentity")
			readExport := path == "kalam.go" && fn.Name.Name == "InspectUploadDestination"
			if !readAdapter && !readExport {
				continue
			}
			ast.Inspect(fn.Body, func(node ast.Node) bool {
				call, ok := node.(*ast.CallExpr)
				if !ok {
					return true
				}
				name := ""
				switch target := call.Fun.(type) {
				case *ast.Ident:
					name = target.Name
				case *ast.SelectorExpr:
					name = target.Sel.Name
				}
				if forbidden[name] {
					t.Fatalf("forbidden call in read route %s: %s", fn.Name.Name, name)
				}
				return true
			})
		}
	}
}
