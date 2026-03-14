// msbrun-hcs — Windows HCS runtime worker for microsandbox.
//
// This binary manages a single HCS compute system (Linux VM) on behalf of
// the Rust control plane. Communication happens over a named pipe.
//
// Usage:
//
//	msbrun-hcs.exe --pipe \\.\pipe\microsandbox-<sandbox_id> --config <path>
//
// The worker reads the compute configuration JSON, creates the HCS compute
// system, attaches VHDs, configures networking, and reports ready status.
// It then listens on the named pipe for control commands (stop, status, attach).
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	log "github.com/sirupsen/logrus"
)

// ComputeConfig is the HCS compute system configuration, matching the Rust
// HcsComputeConfig struct.
type ComputeConfig struct {
	Name              string           `json:"name"`
	KernelPath        string           `json:"kernel_path"`
	InitrdPath        string           `json:"initrd_path"`
	MemoryMiB         uint32           `json:"memory_mib"`
	VcpuCount         uint8            `json:"vcpu_count"`
	ScsiAttachments   []ScsiAttachment `json:"scsi_attachments"`
	Plan9Shares       []Plan9Share     `json:"plan9_shares"`
	NetworkEndpointID *string          `json:"network_endpoint_id"`
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

// PipeMessage represents the named pipe protocol messages.
type PipeMessage struct {
	Type     string  `json:"type"`
	State    string  `json:"state,omitempty"`
	GuestPID *uint32 `json:"guest_pid,omitempty"`
	Success  *bool   `json:"success,omitempty"`
	Error    *string `json:"error,omitempty"`
	Cols     uint16  `json:"cols,omitempty"`
	Rows     uint16  `json:"rows,omitempty"`
}

func main() {
	pipePath := flag.String("pipe", "", "Named pipe path for control channel")
	configPath := flag.String("config", "", "Path to compute configuration JSON")
	flag.Parse()

	if *pipePath == "" || *configPath == "" {
		fmt.Fprintf(os.Stderr, "Usage: msbrun-hcs.exe --pipe <pipe_path> --config <config_path>\n")
		os.Exit(1)
	}

	log.WithFields(log.Fields{
		"pipe":   *pipePath,
		"config": *configPath,
	}).Info("msbrun-hcs worker starting")

	// Read compute configuration.
	configData, err := os.ReadFile(*configPath)
	if err != nil {
		log.WithError(err).Fatal("Failed to read compute configuration")
	}

	var config ComputeConfig
	if err := json.Unmarshal(configData, &config); err != nil {
		log.WithError(err).Fatal("Failed to parse compute configuration")
	}

	log.WithFields(log.Fields{
		"name":   config.Name,
		"memory": config.MemoryMiB,
		"vcpus":  config.VcpuCount,
		"layers": len(config.ScsiAttachments),
		"shares": len(config.Plan9Shares),
	}).Info("Loaded compute configuration")

	// Create HCS compute system.
	if err := createComputeSystem(&config); err != nil {
		log.WithError(err).Fatal("Failed to create HCS compute system")
	}

	log.Info("HCS compute system created, waiting for commands")

	// Handle OS signals for graceful shutdown.
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	// Listen on the named pipe for control commands.
	// For now, just wait for a signal.
	// TODO: Implement named pipe listener with full protocol support.
	<-sigChan

	log.Info("Received shutdown signal, cleaning up")

	// Teardown HCS compute system.
	if err := teardownComputeSystem(&config); err != nil {
		log.WithError(err).Error("Failed to clean up compute system")
		os.Exit(1)
	}

	log.Info("Cleanup complete, exiting")
}

// createComputeSystem creates an HCS compute system using the provided configuration.
// This is a placeholder — the full implementation will use hcsshim's CreateComputeSystem.
func createComputeSystem(config *ComputeConfig) error {
	log.WithField("name", config.Name).Info("Creating HCS compute system")

	// TODO: Implement using hcsshim:
	// 1. Build hcsshim.SchemaV2 document from config
	// 2. Set LinuxKernelDirect boot with kernel and initrd paths
	// 3. Configure memory and CPU limits
	// 4. Add SCSI attachments for layer VHDs
	// 5. Add Plan9 shares for host mounts
	// 6. Set network endpoint if configured
	// 7. Call hcsshim.CreateComputeSystem()
	// 8. Start the compute system

	return nil
}

// teardownComputeSystem terminates the HCS compute system and cleans up resources.
func teardownComputeSystem(config *ComputeConfig) error {
	log.WithField("name", config.Name).Info("Tearing down HCS compute system")

	// TODO: Implement using hcsshim:
	// 1. Terminate the compute system
	// 2. Wait for shutdown
	// 3. Close the compute system handle
	// 4. Detach VHDs
	// 5. Remove Plan9 shares

	return nil
}
