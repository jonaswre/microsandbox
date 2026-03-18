// Package hcs translates microsandbox compute configurations to HCS V2 schema
// documents and manages HCS compute system lifecycle via Win32 HCS API.
package hcs

import (
	"fmt"
	"strconv"
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
	Scsi            map[string]ScsiController  `json:"Scsi,omitempty"`
	Plan9           map[string]Plan9ShareSchema `json:"Plan9,omitempty"`
	NetworkAdapters map[string]NetworkAdapter   `json:"NetworkAdapters,omitempty"`
	ComPorts        map[string]ComPort          `json:"ComPorts,omitempty"`
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
				Scsi:  buildScsiControllers(cfg.ScsiAttachments),
				Plan9: buildPlan9(cfg.Plan9Shares),
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

// buildPlan9 builds the Plan9 share map from share configs.
func buildPlan9(shares []Plan9Share) map[string]Plan9ShareSchema {
	if len(shares) == 0 {
		return nil
	}

	result := make(map[string]Plan9ShareSchema)
	for i, s := range shares {
		name := s.Name
		if name == "" {
			name = fmt.Sprintf("share_%d", i)
		}

		p9 := Plan9ShareSchema{
			Name:     name,
			AccessName: name,
			Path:     s.HostPath,
			Flags:    int32(s.Flags),
			ReadOnly: s.Flags&Plan9FlagReadOnly != 0,
		}

		result[name] = p9
	}

	return result
}
