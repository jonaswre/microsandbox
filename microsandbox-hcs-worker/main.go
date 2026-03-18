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
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	winio "github.com/Microsoft/go-winio"
	log "github.com/sirupsen/logrus"
	"golang.org/x/sys/windows"

	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcn"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcs"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/p9server"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/pipe"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/portal"
)

func main() {
	pipePath := flag.String("pipe", "", "Named pipe path for control channel")
	configPath := flag.String("config", "", "Path to compute configuration JSON")
	flag.Parse()

	if *pipePath == "" || *configPath == "" {
		fmt.Fprintf(os.Stderr, "Usage: msbrun-hcs.exe --pipe <pipe_path> --config <config_path>\n")
		os.Exit(1)
	}

	// Defense-in-depth: verify we're running elevated.
	if !checkElevated() {
		log.Fatal("This worker requires administrator privileges. Please run from an elevated terminal.")
	}

	// Normalize pipe path — ensure proper \\.\pipe\ prefix.
	normalizedPipe := *pipePath
	if !strings.HasPrefix(normalizedPipe, `\\.\pipe\`) {
		normalizedPipe = `\\.\pipe\` + normalizedPipe
	}

	log.WithFields(log.Fields{
		"pipe":   normalizedPipe,
		"config": *configPath,
	}).Info("msbrun-hcs worker starting")

	// Read compute configuration.
	configData, err := os.ReadFile(*configPath)
	if err != nil {
		log.WithError(err).Fatal("Failed to read compute configuration")
	}

	var config hcs.ComputeConfig
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

	// Set up HCN networking if requested.
	var endpointID string
	var epIP string
	var lbIDs []string
	if config.NetworkEndpointID == nil || *config.NetworkEndpointID == "" {
		// No endpoint pre-assigned — set up networking here.
		netID, err := hcn.EnsureNATNetwork("microsandbox-nat", "172.28.0.0/16", "172.28.176.1")
		if err != nil {
			log.WithError(err).Warn("HCN networking unavailable — VM will run without network")
		} else {
			epID, ip, err := hcn.CreateEndpoint(netID, "msb-"+config.Name)
			if err != nil {
				log.WithError(err).Warn("Failed to create HCN endpoint")
			} else {
				endpointID = epID
				epIP = ip
				config.NetworkEndpointID = &epID
				log.WithFields(log.Fields{"endpoint": epID, "ip": epIP}).Info("HCN endpoint created")

				// Create portal load balancer (port 4444).
				if lbID, err := hcn.CreateLoadBalancer(epID, 4444, 4444); err == nil {
					lbIDs = append(lbIDs, lbID)
				} else {
					log.WithError(err).Warn("Failed to create portal load balancer")
				}
			}
		}
	}

	// Build HCS V2 schema document from our config.
	doc := hcs.BuildHcsDocument(&config)

	// Start 9p file servers before VM so guest bootstrap can connect immediately.
	var shareServer *p9server.ShareServer
	if len(config.Plan9Shares) > 0 {
		shareServer = p9server.New(config.Plan9Shares)
		if err := shareServer.Start(); err != nil {
			cleanupHCN(endpointID, lbIDs)
			log.WithError(err).Fatal("Failed to start 9p share servers")
		}
		log.WithField("shares", len(config.Plan9Shares)).Info("Started 9p share servers")
	}

	// Create and start the HCS compute system.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	system, err := hcs.CreateAndStart(ctx, doc, config.Name)
	if err != nil {
		// Clean up networking on HCS failure.
		cleanupHCN(endpointID, lbIDs)
		log.WithError(err).Fatal("Failed to create HCS compute system")
	}

	log.Info("HCS compute system created and started")

	// Start TCP relay for portal proxy if configured.
	var portalRelay *portal.Relay
	if config.PortalHostPort != nil && epIP == "" {
		log.WithField("host_port", *config.PortalHostPort).Error(
			"Portal proxy requested but networking is unavailable (no endpoint IP). " +
				"The server will not be able to reach the portal. Fix HCN networking first.")
	}
	if config.PortalHostPort != nil && epIP != "" {
		relay, err := portal.New(*config.PortalHostPort, epIP, 4444)
		if err != nil {
			log.WithError(err).Warn("Failed to create portal relay")
		} else {
			relay.Start(ctx)
			portalRelay = relay
			log.WithFields(log.Fields{
				"host_port": *config.PortalHostPort,
				"guest":     fmt.Sprintf("%s:4444", epIP),
			}).Info("Portal TCP relay started")
		}
	}

	// Connect to the VM console pipe and relay I/O to host stdin/stdout.
	// When the console pipe closes (VM shutdown), relayConsole cancels the
	// context which unblocks the select loop below.
	if config.ConsolePipePath != "" {
		go relayConsole(ctx, cancel, config.ConsolePipePath)
	}

	// Start the named pipe server in a goroutine.
	pipeServer := pipe.NewServer(normalizedPipe, cancel)
	go func() {
		if err := pipeServer.ListenAndServe(ctx); err != nil {
			log.WithError(err).Error("Named pipe server error")
			cancel()
		}
	}()

	// Handle OS signals for graceful shutdown.
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	// Wait for shutdown signal (OS signal or pipe stop command).
	select {
	case sig := <-sigChan:
		log.WithField("signal", sig).Info("Received shutdown signal")
		cancel()
	case <-ctx.Done():
		log.Info("Shutdown requested via control channel")
	}

	// Stop the pipe server.
	pipeServer.Stop()

	// Teardown the HCS compute system.
	log.Info("Shutting down HCS compute system")
	if err := system.Shutdown(30 * time.Second); err != nil {
		log.WithError(err).Error("Failed to clean up compute system")
	}

	// Stop 9p share servers (connections close when VM shuts down).
	if shareServer != nil {
		shareServer.Stop()
		log.Info("Stopped 9p share servers")
	}

	// Stop portal relay.
	if portalRelay != nil {
		portalRelay.Stop()
		log.Info("Stopped portal relay")
	}

	// Clean up HCN networking resources.
	cleanupHCN(endpointID, lbIDs)

	log.Info("Cleanup complete, exiting")
}

// cleanupHCN removes HCN load balancers and endpoint.
func cleanupHCN(endpointID string, lbIDs []string) {
	for _, id := range lbIDs {
		if err := hcn.DeleteLoadBalancer(id); err != nil {
			log.WithError(err).WithField("id", id).Debug("Failed to delete load balancer")
		}
	}
	if endpointID != "" {
		if err := hcn.DeleteEndpoint(endpointID); err != nil {
			log.WithError(err).WithField("id", endpointID).Debug("Failed to delete endpoint")
		} else {
			log.Info("Cleaned up HCN networking")
		}
	}
}

// relayConsole connects to the HCS console named pipe and bridges it with
// the host's stdin/stdout. Console output goes to os.Stdout; host keyboard
// input goes to the VM's serial console.
//
// When the console pipe closes (VM shutdown), cancel is called to trigger
// worker shutdown. The host terminal is set to raw mode if stdin is a
// terminal, allowing Ctrl+C and other special keys to flow to the guest.
func relayConsole(ctx context.Context, cancel context.CancelFunc, pipePath string) {
	conn, err := connectToConsolePipe(ctx, pipePath)
	if err != nil {
		log.WithError(err).Warn("Could not connect to console pipe — VM output will not be visible")
		return
	}
	defer conn.Close()

	log.WithField("pipe", pipePath).Info("Console pipe connected, relaying I/O")

	// Set terminal to raw mode if stdin is a terminal.
	// This allows interactive use: Ctrl+C, arrow keys, and other special
	// sequences flow through to the guest's serial console unmodified.
	if restore, err := enableRawMode(); err != nil {
		log.WithError(err).Warn("Failed to set terminal to raw mode")
	} else if restore != nil {
		defer restore()
	}

	// Track when console output finishes (pipe EOF = VM exited).
	outputDone := make(chan struct{})

	// Console output → host stdout.
	go func() {
		defer close(outputDone)
		if _, err := io.Copy(os.Stdout, conn); err != nil && ctx.Err() == nil {
			log.WithError(err).Debug("Console output relay stopped")
		}
	}()

	// Host stdin → console input.
	go func() {
		if _, err := io.Copy(conn, os.Stdin); err != nil && ctx.Err() == nil {
			log.WithError(err).Debug("Console input relay stopped")
		}
	}()

	// Wait for either context cancellation or console pipe EOF (VM exit).
	select {
	case <-ctx.Done():
		// Normal shutdown via signal or pipe command.
	case <-outputDone:
		// Console pipe closed — VM has shut down.
		log.Info("Console pipe closed (VM exited), initiating shutdown")
		cancel()
	}
}

// enableRawMode sets the Windows console to raw mode so that key presses are
// sent directly to the guest VM's serial console without local echoing or
// line-buffering. Returns a restore function (nil if stdin is not a terminal).
func enableRawMode() (restore func(), err error) {
	stdinHandle := windows.Handle(os.Stdin.Fd())

	var oldMode uint32
	if err := windows.GetConsoleMode(stdinHandle, &oldMode); err != nil {
		// Not a console (e.g. piped input) — nothing to do.
		return nil, nil
	}

	// Disable echo, line buffering, and Ctrl+C interception so raw bytes
	// flow through to the guest. Enable VT input for escape sequences.
	raw := oldMode &^ (windows.ENABLE_ECHO_INPUT | windows.ENABLE_LINE_INPUT | windows.ENABLE_PROCESSED_INPUT)
	raw |= windows.ENABLE_VIRTUAL_TERMINAL_INPUT

	if err := windows.SetConsoleMode(stdinHandle, raw); err != nil {
		return nil, fmt.Errorf("SetConsoleMode(stdin): %w", err)
	}

	// Enable VT processing on stdout so ANSI escape codes from the guest
	// (colors, cursor movement) are rendered correctly.
	stdoutHandle := windows.Handle(os.Stdout.Fd())
	var outMode uint32
	if err := windows.GetConsoleMode(stdoutHandle, &outMode); err == nil {
		outMode |= windows.ENABLE_VIRTUAL_TERMINAL_PROCESSING
		_ = windows.SetConsoleMode(stdoutHandle, outMode)
	}

	log.Debug("Terminal set to raw mode")
	return func() {
		_ = windows.SetConsoleMode(stdinHandle, oldMode)
		log.Debug("Terminal mode restored")
	}, nil
}

// checkElevated returns true if the current process is running with
// administrator (elevated) privileges.
func checkElevated() bool {
	return windows.GetCurrentProcessToken().IsElevated()
}

// connectToConsolePipe retries connecting to the HCS console named pipe.
// HCS creates the pipe when the compute system starts; there may be a short
// delay before it is available.
func connectToConsolePipe(ctx context.Context, pipePath string) (net.Conn, error) {
	timeout := 100 * time.Millisecond
	for i := 0; i < 30; i++ {
		conn, err := winio.DialPipe(pipePath, &timeout)
		if err == nil {
			return conn, nil
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(500 * time.Millisecond):
		}
	}
	return nil, fmt.Errorf("timeout connecting to console pipe %s", pipePath)
}
