package hcs

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"syscall"
	"time"
	"unsafe"

	log "github.com/sirupsen/logrus"
	"golang.org/x/sys/windows"
)

//--------------------------------------------------------------------------------------------------
// Win32 HCS API syscall bindings (computecore.dll)
//
// Uses the operations-based API available on Windows 10 RS5+ / Windows 11:
//   HcsCreateOperation(context, callback) -> HCS_OPERATION
//   HcsCloseOperation(operation)
//   HcsWaitForOperationResult(operation, timeoutMs, resultDocument) -> HRESULT
//   HcsCreateComputeSystem(id, config, operation, securityDescriptor, computeSystem) -> HRESULT
//   HcsStartComputeSystem(computeSystem, operation, options) -> HRESULT
//   HcsShutDownComputeSystem(computeSystem, operation, options) -> HRESULT
//   HcsTerminateComputeSystem(computeSystem, operation, options) -> HRESULT
//   HcsCloseComputeSystem(computeSystem)
//--------------------------------------------------------------------------------------------------

var (
	modcomputecore = windows.NewLazySystemDLL("computecore.dll")

	procHcsCreateOperation        = modcomputecore.NewProc("HcsCreateOperation")
	procHcsCloseOperation         = modcomputecore.NewProc("HcsCloseOperation")
	procHcsWaitForOperationResult = modcomputecore.NewProc("HcsWaitForOperationResult")
	procHcsCreateComputeSystem    = modcomputecore.NewProc("HcsCreateComputeSystem")
	procHcsStartComputeSystem     = modcomputecore.NewProc("HcsStartComputeSystem")
	procHcsShutDownComputeSystem  = modcomputecore.NewProc("HcsShutDownComputeSystem")
	procHcsTerminateComputeSystem = modcomputecore.NewProc("HcsTerminateComputeSystem")
	procHcsCloseComputeSystem     = modcomputecore.NewProc("HcsCloseComputeSystem")
)

// hcsSystem is an opaque HCS_SYSTEM handle.
type hcsSystem uintptr

// hcsOperation is an opaque HCS_OPERATION handle.
type hcsOperation uintptr

func createOperation() (hcsOperation, error) {
	r, _, _ := procHcsCreateOperation.Call(0, 0)
	if r == 0 {
		return 0, fmt.Errorf("HcsCreateOperation returned NULL")
	}
	return hcsOperation(r), nil
}

func closeOperation(op hcsOperation) {
	if op != 0 {
		procHcsCloseOperation.Call(uintptr(op))
	}
}

// waitForOperation waits for an async HCS operation and returns the result document.
func waitForOperation(op hcsOperation, timeoutMs uint32) (string, error) {
	var resultDoc *uint16
	r0, _, _ := syscall.SyscallN(procHcsWaitForOperationResult.Addr(),
		uintptr(op),
		uintptr(timeoutMs),
		uintptr(unsafe.Pointer(&resultDoc)),
	)

	var detail string
	if resultDoc != nil {
		detail = windows.UTF16PtrToString(resultDoc)
		windows.CoTaskMemFree(unsafe.Pointer(resultDoc))
	}

	if int32(r0) < 0 {
		errMsg := fmt.Sprintf("HRESULT 0x%08x", uint32(r0))
		if detail != "" {
			errMsg += ": " + detail
		}
		return detail, fmt.Errorf("%s", errMsg)
	}
	return detail, nil
}

//--------------------------------------------------------------------------------------------------
// System type
//--------------------------------------------------------------------------------------------------

// System wraps an HCS compute system handle.
type System struct {
	handle hcsSystem
	name   string
	mu     sync.Mutex
}

// CreateAndStart creates an HCS compute system from a schema document, then starts it.
func CreateAndStart(ctx context.Context, doc *HcsDocument, name string) (*System, error) {
	docJSON, err := json.Marshal(doc)
	if err != nil {
		return nil, fmt.Errorf("marshal HCS document: %w", err)
	}

	log.WithFields(log.Fields{
		"name":     name,
		"doc_size": len(docJSON),
	}).Info("Creating HCS compute system")

	idUTF16, _ := syscall.UTF16PtrFromString(name)
	configUTF16, _ := syscall.UTF16PtrFromString(string(docJSON))

	// Create operation for async tracking.
	op, err := createOperation()
	if err != nil {
		return nil, err
	}
	defer closeOperation(op)

	// HcsCreateComputeSystem(id, config, operation, securityDescriptor, computeSystem) -> HRESULT
	var sysHandle hcsSystem
	r0, _, _ := syscall.SyscallN(procHcsCreateComputeSystem.Addr(),
		uintptr(unsafe.Pointer(idUTF16)),
		uintptr(unsafe.Pointer(configUTF16)),
		uintptr(op),
		0, // securityDescriptor
		uintptr(unsafe.Pointer(&sysHandle)),
	)
	if int32(r0) < 0 {
		return nil, fmt.Errorf("HcsCreateComputeSystem failed: HRESULT 0x%08x", uint32(r0))
	}

	// Wait for creation to complete.
	if _, err := waitForOperation(op, 60000); err != nil {
		if sysHandle != 0 {
			closeComputeSystem(sysHandle)
		}
		return nil, fmt.Errorf("create compute system: %w", err)
	}

	log.WithField("name", name).Info("HCS compute system created, starting")

	// Start the compute system.
	startOp, err := createOperation()
	if err != nil {
		closeComputeSystem(sysHandle)
		return nil, err
	}
	defer closeOperation(startOp)

	r0, _, _ = syscall.SyscallN(procHcsStartComputeSystem.Addr(),
		uintptr(sysHandle),
		uintptr(startOp),
		0, // options
	)
	if int32(r0) < 0 {
		closeComputeSystem(sysHandle)
		return nil, fmt.Errorf("HcsStartComputeSystem failed: HRESULT 0x%08x", uint32(r0))
	}

	if _, err := waitForOperation(startOp, 60000); err != nil {
		closeComputeSystem(sysHandle)
		return nil, fmt.Errorf("start compute system: %w", err)
	}

	log.WithField("name", name).Info("HCS compute system started")
	return &System{handle: sysHandle, name: name}, nil
}

// Shutdown gracefully shuts down the compute system.
func (s *System) Shutdown(timeout time.Duration) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.handle == 0 {
		return nil
	}

	log.WithField("name", s.name).Info("Shutting down HCS compute system")

	op, err := createOperation()
	if err != nil {
		return s.terminateLocked()
	}
	defer closeOperation(op)

	r0, _, _ := syscall.SyscallN(procHcsShutDownComputeSystem.Addr(),
		uintptr(s.handle),
		uintptr(op),
		0,
	)
	if int32(r0) < 0 {
		log.WithField("hresult", fmt.Sprintf("0x%08x", uint32(r0))).Warn("Graceful shutdown failed, terminating")
		return s.terminateLocked()
	}

	timeoutMs := uint32(timeout.Milliseconds())
	if _, err := waitForOperation(op, timeoutMs); err != nil {
		log.WithError(err).Warn("Shutdown wait failed, terminating")
		return s.terminateLocked()
	}

	closeComputeSystem(s.handle)
	s.handle = 0
	log.WithField("name", s.name).Info("HCS compute system shut down")
	return nil
}

func (s *System) terminateLocked() error {
	op, _ := createOperation()
	if op != 0 {
		defer closeOperation(op)
	}

	syscall.SyscallN(procHcsTerminateComputeSystem.Addr(),
		uintptr(s.handle),
		uintptr(op),
		0,
	)
	if op != 0 {
		waitForOperation(op, 10000)
	}

	closeComputeSystem(s.handle)
	s.handle = 0
	log.WithField("name", s.name).Info("HCS compute system terminated")
	return nil
}

// Close releases the compute system handle.
func (s *System) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.handle != 0 {
		closeComputeSystem(s.handle)
		s.handle = 0
	}
}

func closeComputeSystem(handle hcsSystem) {
	if handle != 0 {
		procHcsCloseComputeSystem.Call(uintptr(handle))
	}
}
