package hcs

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestBuildHcsDocument_Basic(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test~sandbox",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  512,
		VcpuCount:  2,
	}

	doc := BuildHcsDocument(cfg)

	if doc.Owner != "microsandbox" {
		t.Errorf("expected owner 'microsandbox', got %q", doc.Owner)
	}
	if doc.SchemaVersion.Major != 2 || doc.SchemaVersion.Minor != 1 {
		t.Errorf("expected schema version 2.1, got %d.%d", doc.SchemaVersion.Major, doc.SchemaVersion.Minor)
	}
	if doc.VirtualMachine.ComputeTopology.Memory.SizeInMB != 512 {
		t.Errorf("expected 512 MB, got %d", doc.VirtualMachine.ComputeTopology.Memory.SizeInMB)
	}
	if doc.VirtualMachine.ComputeTopology.Processor.Count != 2 {
		t.Errorf("expected 2 vCPUs, got %d", doc.VirtualMachine.ComputeTopology.Processor.Count)
	}
	if doc.VirtualMachine.Chipset.LinuxKernelDirect == nil {
		t.Fatal("LinuxKernelDirect should not be nil")
	}
	if doc.VirtualMachine.Chipset.LinuxKernelDirect.KernelFilePath != `C:\boot\kernel` {
		t.Errorf("unexpected kernel path: %s", doc.VirtualMachine.Chipset.LinuxKernelDirect.KernelFilePath)
	}
	// LinuxKernelDirect in Chipset implies Linux guest — no separate GuestState needed.
}

func TestBuildHcsDocument_ScsiAttachments(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
		ScsiAttachments: []ScsiAttachment{
			{Path: `C:\layers\layer0.vhd`, ReadOnly: true, Controller: 0, Lun: 0},
			{Path: `C:\layers\layer1.vhd`, ReadOnly: true, Controller: 0, Lun: 1},
			{Path: `C:\sandbox\scratch.vhdx`, ReadOnly: false, Controller: 0, Lun: 2},
		},
	}

	doc := BuildHcsDocument(cfg)

	scsi := doc.VirtualMachine.Devices.Scsi
	if scsi == nil {
		t.Fatal("SCSI controllers should not be nil")
	}

	ctrl0, ok := scsi["0"]
	if !ok {
		t.Fatal("expected controller '0'")
	}

	if len(ctrl0.Attachments) != 3 {
		t.Fatalf("expected 3 attachments, got %d", len(ctrl0.Attachments))
	}

	// Check LUN 0 is read-only.
	att0, ok := ctrl0.Attachments["0"]
	if !ok {
		t.Fatal("expected LUN '0'")
	}
	if !att0.ReadOnly {
		t.Error("LUN 0 should be read-only")
	}
	if att0.Type != "VirtualDisk" {
		t.Errorf("expected type 'VirtualDisk', got %q", att0.Type)
	}

	// Check LUN 2 is read-write.
	att2, ok := ctrl0.Attachments["2"]
	if !ok {
		t.Fatal("expected LUN '2'")
	}
	if att2.ReadOnly {
		t.Error("LUN 2 (scratch) should be read-write")
	}
}

func TestBuildHcsDocument_Plan9Shares(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
		Plan9Shares: []Plan9Share{
			{Name: "control", HostPath: `C:\runtime\sandbox`, GuestPath: "/run/microsandbox/control", Flags: Plan9FlagReadOnly | Plan9FlagLinuxMetadata | Plan9FlagCaseSensitive},
			{Name: "mount_0", HostPath: `C:\Users\jonas\project`, GuestPath: "/workspace", Flags: Plan9FlagLinuxMetadata | Plan9FlagCaseSensitive},
		},
	}

	doc := BuildHcsDocument(cfg)

	plan9 := doc.VirtualMachine.Devices.Plan9
	if plan9 == nil {
		t.Fatal("Plan9 device should not be nil")
	}
	if len(plan9.Shares) != 2 {
		t.Fatalf("expected 2 Plan9 shares, got %d", len(plan9.Shares))
	}

	// Find shares by name.
	var ctrl, mount0 *Plan9ShareSchema
	for i := range plan9.Shares {
		switch plan9.Shares[i].Name {
		case "control":
			ctrl = &plan9.Shares[i]
		case "mount_0":
			mount0 = &plan9.Shares[i]
		}
	}

	if ctrl == nil {
		t.Fatal("expected 'control' share")
	}
	if !ctrl.ReadOnly {
		t.Error("control share should be read-only")
	}

	if mount0 == nil {
		t.Fatal("expected 'mount_0' share")
	}
	if mount0.ReadOnly {
		t.Error("mount_0 should not be read-only")
	}
	if mount0.Path != `C:\Users\jonas\project` {
		t.Errorf("unexpected host path: %s", mount0.Path)
	}
}

func TestBuildHcsDocument_Network(t *testing.T) {
	endpointID := "abc-123-def"
	cfg := &ComputeConfig{
		Name:              "test",
		KernelPath:        `C:\boot\kernel`,
		InitrdPath:        `C:\boot\rootfs.vhd`,
		MemoryMiB:         256,
		VcpuCount:         1,
		NetworkEndpointID: &endpointID,
	}

	doc := BuildHcsDocument(cfg)

	adapters := doc.VirtualMachine.Devices.NetworkAdapters
	if adapters == nil {
		t.Fatal("NetworkAdapters should not be nil when endpoint is set")
	}

	eth0, ok := adapters["eth0"]
	if !ok {
		t.Fatal("expected 'eth0' adapter")
	}
	if eth0.EndpointID != endpointID {
		t.Errorf("expected endpoint ID %q, got %q", endpointID, eth0.EndpointID)
	}
}

func TestBuildHcsDocument_NoNetwork(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
	}

	doc := BuildHcsDocument(cfg)
	if doc.VirtualMachine.Devices.NetworkAdapters != nil {
		t.Error("NetworkAdapters should be nil when no endpoint is set")
	}
}

func TestBuildHcsDocument_KernelCmdLine(t *testing.T) {
	cfg := &ComputeConfig{
		Name:          "test",
		KernelPath:    `C:\boot\kernel`,
		InitrdPath:    `C:\boot\rootfs.vhd`,
		KernelCmdLine: "init=/bootstrap console=ttyS0 panic=1 layers=2",
		MemoryMiB:     256,
		VcpuCount:     1,
	}

	doc := BuildHcsDocument(cfg)

	cmdline := doc.VirtualMachine.Chipset.LinuxKernelDirect.KernelCmdLine
	if cmdline != "init=/bootstrap console=ttyS0 panic=1 layers=2" {
		t.Errorf("unexpected kernel cmdline: %q", cmdline)
	}
}

func TestBuildHcsDocument_ComPorts(t *testing.T) {
	cfg := &ComputeConfig{
		Name:            "test",
		KernelPath:      `C:\boot\kernel`,
		InitrdPath:      `C:\boot\rootfs.vhd`,
		MemoryMiB:       256,
		VcpuCount:       1,
		ConsolePipePath: `\\.\pipe\microsandbox-console-test`,
	}

	doc := BuildHcsDocument(cfg)

	comPorts := doc.VirtualMachine.Devices.ComPorts
	if comPorts == nil {
		t.Fatal("ComPorts should not be nil when ConsolePipePath is set")
	}

	com0, ok := comPorts["0"]
	if !ok {
		t.Fatal("expected COM port '0'")
	}
	if com0.NamedPipe != `\\.\pipe\microsandbox-console-test` {
		t.Errorf("unexpected pipe path: %q", com0.NamedPipe)
	}
}

func TestBuildHcsDocument_HvSockets(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
		Plan9Shares: []Plan9Share{
			{Name: "mount_0", HostPath: `C:\project`, GuestPath: "/workspace", Flags: Plan9FlagLinuxMetadata | Plan9FlagCaseSensitive},
			{Name: "mount_1", HostPath: `C:\data`, GuestPath: "/data", Flags: Plan9FlagReadOnly | Plan9FlagLinuxMetadata | Plan9FlagCaseSensitive},
		},
	}

	doc := BuildHcsDocument(cfg)

	hvSocket := doc.VirtualMachine.Devices.HvSocket
	if hvSocket == nil || hvSocket.HvSocketConfig == nil {
		t.Fatal("HvSocket device should not be nil when Plan9 shares are present")
	}
	hvsockets := hvSocket.HvSocketConfig
	if len(hvsockets.ServiceTable) != 2 {
		t.Fatalf("expected 2 service table entries, got %d", len(hvsockets.ServiceTable))
	}

	// Verify GUID for port 50000 (0xC350).
	entry0, ok := hvsockets.ServiceTable["0000c350-facb-11e6-bd58-64006a7986d3"]
	if !ok {
		t.Fatal("expected service table entry for port 50000")
	}
	if entry0.BindSecurityDescriptor != "D:P(A;;FA;;;WD)" {
		t.Errorf("unexpected bind SD: %s", entry0.BindSecurityDescriptor)
	}
	if entry0.ConnectSecurityDescriptor != "D:P(A;;FA;;;WD)" {
		t.Errorf("unexpected connect SD: %s", entry0.ConnectSecurityDescriptor)
	}
	if !entry0.AllowWildcardBinds {
		t.Error("AllowWildcardBinds should be true")
	}

	// Verify GUID for port 50001 (0xC351).
	_, ok = hvsockets.ServiceTable["0000c351-facb-11e6-bd58-64006a7986d3"]
	if !ok {
		t.Fatal("expected service table entry for port 50001")
	}
}

func TestBuildHcsDocument_HvSockets_Empty(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
	}

	doc := BuildHcsDocument(cfg)
	if doc.VirtualMachine.Devices.HvSocket != nil {
		t.Error("HvSocket device should be nil when no Plan9 shares are present")
	}
}

func TestBuildHcsDocument_HvSockets_JSONRoundtrip(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
		Plan9Shares: []Plan9Share{
			{Name: "mount_0", HostPath: `C:\project`, GuestPath: "/workspace", Flags: 0xC},
		},
	}

	doc := BuildHcsDocument(cfg)
	data, err := json.Marshal(doc)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	// Verify HvSocket key is present in JSON.
	jsonStr := string(data)
	if !strings.Contains(jsonStr, "HvSocket") {
		t.Error("JSON should contain HvSocket key")
	}
	if !strings.Contains(jsonStr, "ServiceTable") {
		t.Error("JSON should contain ServiceTable key")
	}

	// Roundtrip.
	var roundtrip HcsDocument
	if err := json.Unmarshal(data, &roundtrip); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}
	if roundtrip.VirtualMachine.Devices.HvSocket == nil {
		t.Fatal("HvSocket should survive JSON roundtrip")
	}
	if len(roundtrip.VirtualMachine.Devices.HvSocket.HvSocketConfig.ServiceTable) != 1 {
		t.Errorf("expected 1 service table entry after roundtrip, got %d",
			len(roundtrip.VirtualMachine.Devices.HvSocket.HvSocketConfig.ServiceTable))
	}
}

func TestBuildHcsDocument_NoComPorts(t *testing.T) {
	cfg := &ComputeConfig{
		Name:       "test",
		KernelPath: `C:\boot\kernel`,
		InitrdPath: `C:\boot\rootfs.vhd`,
		MemoryMiB:  256,
		VcpuCount:  1,
	}

	doc := BuildHcsDocument(cfg)

	if doc.VirtualMachine.Devices.ComPorts != nil {
		t.Error("ComPorts should be nil when ConsolePipePath is empty")
	}
}

func TestComputeConfig_ConsolePipeDeserialization(t *testing.T) {
	// Verify that the Rust-generated JSON with console_pipe_path deserializes correctly.
	input := `{
		"name": "test~sandbox",
		"kernel_path": "C:\\boot\\kernel",
		"initrd_path": "C:\\boot\\rootfs.vhd",
		"kernel_cmdline": "init=/bootstrap console=ttyS0 panic=1 layers=1",
		"memory_mib": 512,
		"vcpu_count": 2,
		"scsi_attachments": [],
		"plan9_shares": [],
		"network_endpoint_id": null,
		"console_pipe_path": "\\\\.\\pipe\\microsandbox-console-test~sandbox"
	}`

	var config ComputeConfig
	if err := json.Unmarshal([]byte(input), &config); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if config.KernelCmdLine != "init=/bootstrap console=ttyS0 panic=1 layers=1" {
		t.Errorf("unexpected kernel cmdline: %q", config.KernelCmdLine)
	}
	if config.ConsolePipePath != `\\.\pipe\microsandbox-console-test~sandbox` {
		t.Errorf("unexpected console pipe path: %q", config.ConsolePipePath)
	}
}

func TestComputeConfig_PortalHostPortDeserialization(t *testing.T) {
	input := `{
		"name": "test",
		"kernel_path": "C:\\boot\\kernel",
		"initrd_path": "C:\\boot\\rootfs.vhd",
		"kernel_cmdline": "init=/bootstrap console=ttyS0 panic=1 layers=1",
		"memory_mib": 256,
		"vcpu_count": 1,
		"scsi_attachments": [],
		"plan9_shares": [],
		"network_endpoint_id": null,
		"portal_host_port": 52345
	}`

	var config ComputeConfig
	if err := json.Unmarshal([]byte(input), &config); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if config.PortalHostPort == nil {
		t.Fatal("PortalHostPort should not be nil")
	}
	if *config.PortalHostPort != 52345 {
		t.Errorf("expected 52345, got %d", *config.PortalHostPort)
	}
}

func TestComputeConfig_PortalHostPortOmitted(t *testing.T) {
	input := `{
		"name": "test",
		"kernel_path": "C:\\boot\\kernel",
		"initrd_path": "C:\\boot\\rootfs.vhd",
		"kernel_cmdline": "init=/bootstrap console=ttyS0 panic=1 layers=1",
		"memory_mib": 256,
		"vcpu_count": 1,
		"scsi_attachments": [],
		"plan9_shares": [],
		"network_endpoint_id": null
	}`

	var config ComputeConfig
	if err := json.Unmarshal([]byte(input), &config); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if config.PortalHostPort != nil {
		t.Error("PortalHostPort should be nil when omitted")
	}
}

func TestBuildHcsDocument_JSONRoundtrip(t *testing.T) {
	endpointID := "ep-123"
	cfg := &ComputeConfig{
		Name:       "project~sandbox",
		KernelPath: `C:\runtime\uvm\0.2.6\kernel`,
		InitrdPath: `C:\runtime\uvm\0.2.6\rootfs.vhd`,
		MemoryMiB:  1024,
		VcpuCount:  4,
		ScsiAttachments: []ScsiAttachment{
			{Path: `C:\cache\layer.vhd`, ReadOnly: true, Controller: 0, Lun: 0},
		},
		Plan9Shares: []Plan9Share{
			{Name: "control", HostPath: `C:\runtime\sandbox`, GuestPath: "/run/microsandbox/control", Flags: 0xD},
		},
		NetworkEndpointID: &endpointID,
	}

	doc := BuildHcsDocument(cfg)

	// Serialize to JSON and back.
	data, err := json.Marshal(doc)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var roundtrip HcsDocument
	if err := json.Unmarshal(data, &roundtrip); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if roundtrip.Owner != "microsandbox" {
		t.Errorf("expected owner 'microsandbox', got %q", roundtrip.Owner)
	}
	if roundtrip.VirtualMachine.ComputeTopology.Memory.SizeInMB != 1024 {
		t.Errorf("expected 1024 MB, got %d", roundtrip.VirtualMachine.ComputeTopology.Memory.SizeInMB)
	}
}
