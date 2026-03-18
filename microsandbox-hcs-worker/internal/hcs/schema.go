// Package hcs translates microsandbox compute configurations to HCS V2 schema
// documents and manages HCS compute system lifecycle via Win32 HCS API.
package hcs

import (
	"fmt"
	"strconv"

	"github.com/Microsoft/go-winio/pkg/guid"
)

// ComputeConfig is the HCS compute system configuration, matching the Rust
// HcsComputeConfig struct.
type ComputeConfig struct {
	Name              string           `json:"name"`
	KernelPath        string           `json:"kernel_path"`
	InitrdPath        string           `json:"initrd_path"`
	KernelCmdLine     string           `json:"kernel_cmdline"`
	MemoryMiB         uint32           `json:"memory_mib"`
	VcpuCount         uint8            `json:"vcpu_count"`
	ScsiAttachments   []ScsiAttachment `json:"scsi_attachments"`
	Plan9Shares       []Plan9Share     `json:"plan9_shares"`
	NetworkEndpointID *string          `json:"network_endpoint_id"`
	ConsolePipePath   string           `json:"console_pipe_path,omitempty"`
	PortalHostPort    *uint16          `json:"portal_host_port,omitempty"`
}

// ScsiAttachment represents a VHD to attach via SCSI.
type ScsiAttachment struct {
	Path       string `json:"path"`
	ReadOnly   bool   `json:"readonly"`
	Controller uint32 `json:"controller"`
	Lun        uint32 `json:"lun"`
}

// Plan9Share represents a host directory mount.
type Plan9Share struct {
	Name      string `json:"name"`
	HostPath  string `json:"host_path"`
	GuestPath string `json:"guest_path"`
	Flags     uint32 `json:"flags"`
}

// Plan9 share flag constants (must match Rust definitions).
const (
	Plan9FlagReadOnly      uint32 = 0x1
	Plan9FlagLinuxMetadata uint32 = 0x4
	Plan9FlagCaseSensitive uint32 = 0x8
)

// BaseMountPort is the first vsock port used for 9p mount shares.
const BaseMountPort uint32 = 50000

// hvSocketSecurityDescriptor grants full access to everyone (SDDL: Everyone = WD).
// Required for HvSocket service entries so the host p9 server can bind and the
// guest can connect via AF_VSOCK.
const hvSocketSecurityDescriptor = "D:P(A;;FA;;;WD)"

// PortToServiceGUID converts a vsock port number to the Hyper-V socket service GUID.
// Uses the standard template: {XXXXXXXX-facb-11e6-bd58-64006a7986d3} where XXXXXXXX = hex port.
// This is the same GUID format used by WSL2 for vsock-to-HvSocket mapping.
func PortToServiceGUID(port uint32) guid.GUID {
	return guid.GUID{
		Data1: port,
		Data2: 0xfacb,
		Data3: 0x11e6,
		Data4: [8]byte{0xbd, 0x58, 0x64, 0x00, 0x6a, 0x79, 0x86, 0xd3},
	}
}

//--------------------------------------------------------------------------------------------------
// HCS V2 Schema Types
//--------------------------------------------------------------------------------------------------
// These types define the HCS V2 JSON schema for compute system creation.
// They match the schema expected by HcsCreateComputeSystem in computecore.dll.

// HcsDocument is the top-level HCS V2 compute system configuration.
type HcsDocument struct {
	Owner                             string          `json:"Owner"`
	SchemaVersion                     *SchemaVersion  `json:"SchemaVersion"`
	ShouldTerminateOnLastHandleClosed bool            `json:"ShouldTerminateOnLastHandleClosed"`
	VirtualMachine                    *VirtualMachine `json:"VirtualMachine"`
}

// SchemaVersion identifies the HCS schema version.
type SchemaVersion struct {
	Major int `json:"Major"`
	Minor int `json:"Minor"`
}

// VirtualMachine describes the VM configuration.
type VirtualMachine struct {
	StopOnReset     bool             `json:"StopOnReset"`
	Chipset         *Chipset         `json:"Chipset"`
	ComputeTopology *ComputeTopology `json:"ComputeTopology"`
	Devices         *Devices         `json:"Devices"`
}

// Chipset describes the VM chipset configuration.
type Chipset struct {
	LinuxKernelDirect *LinuxKernelDirect `json:"LinuxKernelDirect,omitempty"`
}

// LinuxKernelDirect configures direct Linux kernel boot.
type LinuxKernelDirect struct {
	KernelFilePath string `json:"KernelFilePath"`
	InitRdPath     string `json:"InitRdPath,omitempty"`
	KernelCmdLine  string `json:"KernelCmdLine,omitempty"`
}

// ComputeTopology describes resource allocation.
type ComputeTopology struct {
	Memory    *Memory    `json:"Memory"`
	Processor *Processor `json:"Processor"`
}

// Memory describes memory allocation.
type Memory struct {
	SizeInMB uint64 `json:"SizeInMB"`
}

// Processor describes CPU allocation.
type Processor struct {
	Count uint32 `json:"Count"`
}

// Devices describes device attachments.
type Devices struct {
	Scsi            map[string]ScsiController `json:"Scsi,omitempty"`
	HvSocket        *HvSocketDevice           `json:"HvSocket,omitempty"`
	Plan9           *Plan9Device              `json:"Plan9,omitempty"`
	VirtioFs        *VirtioFsDevice           `json:"VirtioFs,omitempty"`
	NetworkAdapters map[string]NetworkAdapter  `json:"NetworkAdapters,omitempty"`
	ComPorts        map[string]ComPort         `json:"ComPorts,omitempty"`
}

// HvSocketDevice wraps HvSocket configuration in the HCS V2 schema.
// Placed under Devices, not VirtualMachine.
type HvSocketDevice struct {
	HvSocketConfig *HvSocketSystemConfig `json:"HvSocketConfig,omitempty"`
}

// Plan9Device wraps Plan9 shares in the HCS V2 schema structure.
// HCS expects { "Plan9": { "Shares": [...] } }, not a flat map.
type Plan9Device struct {
	Shares []Plan9ShareSchema `json:"Shares,omitempty"`
}

// VirtioFsDevice wraps VirtioFS shares in the HCS V2 schema structure.
type VirtioFsDevice struct {
	Shares []VirtioFsShareSchema `json:"Shares,omitempty"`
}

// VirtioFsShareSchema is the HCS schema VirtioFS share config.
type VirtioFsShareSchema struct {
	Name string `json:"Name,omitempty"`
	Path string `json:"Path,omitempty"`
}

// ComPort describes a serial port redirected to a named pipe.
type ComPort struct {
	NamedPipe string `json:"NamedPipe,omitempty"`
}

// ScsiController describes a SCSI controller with its attachments.
type ScsiController struct {
	Attachments map[string]Attachment `json:"Attachments"`
}

// Attachment describes a SCSI disk attachment.
type Attachment struct {
	Path     string `json:"Path"`
	Type     string `json:"Type"`
	ReadOnly bool   `json:"ReadOnly,omitempty"`
}

// Plan9ShareSchema is the HCS schema Plan9 share config.
type Plan9ShareSchema struct {
	Name     string `json:"Name,omitempty"`
	AccessName string `json:"AccessName,omitempty"`
	Path     string `json:"Path,omitempty"`
	Port     int32  `json:"Port,omitempty"`
	Flags    int32  `json:"Flags,omitempty"`
	ReadOnly bool   `json:"ReadOnly,omitempty"`
}

// NetworkAdapter describes a network adapter.
type NetworkAdapter struct {
	EndpointID string `json:"EndpointId"`
}

// HvSocketSystemConfig configures Hyper-V socket services for the VM.
// Used to register vsock service GUIDs that the host can listen on
// and the guest can connect to via AF_VSOCK.
type HvSocketSystemConfig struct {
	ServiceTable map[string]HvSocketServiceEntry `json:"ServiceTable,omitempty"`
}

// HvSocketServiceEntry configures a single HvSocket service.
type HvSocketServiceEntry struct {
	BindSecurityDescriptor    string `json:"BindSecurityDescriptor,omitempty"`
	ConnectSecurityDescriptor string `json:"ConnectSecurityDescriptor,omitempty"`
	AllowWildcardBinds        bool   `json:"AllowWildcardBinds,omitempty"`
}

//--------------------------------------------------------------------------------------------------
// Schema Builder
//--------------------------------------------------------------------------------------------------

// BuildHcsDocument translates a ComputeConfig into an HCS V2 schema document.
func BuildHcsDocument(cfg *ComputeConfig) *HcsDocument {
	doc := &HcsDocument{
		Owner: "microsandbox",
		SchemaVersion: &SchemaVersion{
			Major: 2,
			Minor: 1,
		},
		ShouldTerminateOnLastHandleClosed: true,
		VirtualMachine: &VirtualMachine{
			StopOnReset: true,
			Chipset: &Chipset{
				LinuxKernelDirect: &LinuxKernelDirect{
					KernelFilePath: cfg.KernelPath,
					InitRdPath:     cfg.InitrdPath,
					KernelCmdLine:  cfg.KernelCmdLine,
				},
			},
			ComputeTopology: &ComputeTopology{
				Memory: &Memory{
					SizeInMB: uint64(cfg.MemoryMiB),
				},
				Processor: &Processor{
					Count: uint32(cfg.VcpuCount),
				},
			},
			Devices: &Devices{
				Scsi:     buildScsiControllers(cfg.ScsiAttachments),
				HvSocket: buildHvSocket(cfg.Plan9Shares),
				Plan9:    buildPlan9(cfg.Plan9Shares),
			},
		},
	}

	// Configure COM port for serial console redirection.
	if cfg.ConsolePipePath != "" {
		doc.VirtualMachine.Devices.ComPorts = map[string]ComPort{
			"0": {NamedPipe: cfg.ConsolePipePath},
		}
	}

	// Configure network adapter if endpoint is provided.
	if cfg.NetworkEndpointID != nil && *cfg.NetworkEndpointID != "" {
		doc.VirtualMachine.Devices.NetworkAdapters = map[string]NetworkAdapter{
			"eth0": {
				EndpointID: *cfg.NetworkEndpointID,
			},
		}
	}

	return doc
}

// buildScsiControllers builds the SCSI controller map from attachment configs.
// HCS V2 uses string keys for both controllers and LUNs.
func buildScsiControllers(attachments []ScsiAttachment) map[string]ScsiController {
	if len(attachments) == 0 {
		return nil
	}

	// Group attachments by controller.
	controllers := make(map[uint32]map[uint32]ScsiAttachment)
	for _, a := range attachments {
		if controllers[a.Controller] == nil {
			controllers[a.Controller] = make(map[uint32]ScsiAttachment)
		}
		controllers[a.Controller][a.Lun] = a
	}

	result := make(map[string]ScsiController)
	for ctrlID, luns := range controllers {
		scsiAttachments := make(map[string]Attachment)
		for lunID, a := range luns {
			att := Attachment{
				Path:     a.Path,
				Type:     "VirtualDisk",
				ReadOnly: a.ReadOnly,
			}
			scsiAttachments[strconv.FormatUint(uint64(lunID), 10)] = att
		}
		result[strconv.FormatUint(uint64(ctrlID), 10)] = ScsiController{
			Attachments: scsiAttachments,
		}
	}

	return result
}

// buildPlan9 builds the Plan9 device from share configs.
func buildPlan9(shares []Plan9Share) *Plan9Device {
	if len(shares) == 0 {
		return nil
	}

	var p9shares []Plan9ShareSchema
	for i, s := range shares {
		name := s.Name
		if name == "" {
			name = fmt.Sprintf("share_%d", i)
		}

		p9shares = append(p9shares, Plan9ShareSchema{
			Name:       name,
			AccessName: name,
			Path:       s.HostPath,
			Port:       int32(i + 1), // HCS requires a non-zero port per share
			Flags:      int32(s.Flags),
			ReadOnly:   s.Flags&Plan9FlagReadOnly != 0,
		})
	}

	return &Plan9Device{Shares: p9shares}
}

// buildHvSocket builds the HvSocket device with service table for 9p mounts over vsock.
// Each Plan9 share gets a service table entry keyed by its vsock port GUID.
func buildHvSocket(shares []Plan9Share) *HvSocketDevice {
	if len(shares) == 0 {
		return nil
	}

	table := make(map[string]HvSocketServiceEntry)
	for i := range shares {
		port := BaseMountPort + uint32(i)
		guidStr := PortToServiceGUID(port).String()
		table[guidStr] = HvSocketServiceEntry{
			BindSecurityDescriptor:    hvSocketSecurityDescriptor,
			ConnectSecurityDescriptor: hvSocketSecurityDescriptor,
			AllowWildcardBinds:        true,
		}
	}

	return &HvSocketDevice{
		HvSocketConfig: &HvSocketSystemConfig{ServiceTable: table},
	}
}

// buildVirtioFs builds the VirtioFS device from share configs.
// VirtioFS is the modern alternative to Plan9 for host directory sharing.
// NOTE: VirtioFS causes HCS Construct errors on Windows 11 build 26200.
// Kept for future use when newer builds support it.
func buildVirtioFs(shares []Plan9Share) *VirtioFsDevice { //nolint:unused
	if len(shares) == 0 {
		return nil
	}

	var vfsShares []VirtioFsShareSchema
	for i, s := range shares {
		name := s.Name
		if name == "" {
			name = fmt.Sprintf("share_%d", i)
		}
		vfsShares = append(vfsShares, VirtioFsShareSchema{
			Name: name,
			Path: s.HostPath,
		})
	}

	return &VirtioFsDevice{Shares: vfsShares}
}
