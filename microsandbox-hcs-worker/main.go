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

	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcn"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcs"
	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/pipe"
)

func main() {
	pipePath := flag.String("pipe", "", "Named pipe path for control channel")
	configPath := flag.String("config", "", "Path to compute configuration JSON")
	flag.Parse()

	if *pipePath == "" || *configPath == "" {
		fmt.Fprintf(os.Stderr, "Usage: msbrun-hcs.exe --pipe <pipe_path> --config <config_path>\n")
		os.Exit(1)
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
	var lbIDs []string
	if config.NetworkEndpointID == nil || *config.NetworkEndpointID == "" {
		// No endpoint pre-assigned — set up networking here.
		netID, err := hcn.EnsureNATNetwork("microsandbox-nat", "172.28.0.0/16", "172.28.176.1")
		if err != nil {
			log.WithError(err).Warn("HCN networking unavailable — VM will run without network")
		} else {
			epID, epIP, err := hcn.CreateEndpoint(netID, "msb-"+config.Name)
			if err != nil {
				log.WithError(err).Warn("Failed to create HCN endpoint")
			} else {
				endpointID = epID
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

	// Connect to the VM console pipe and relay I/O to host stdin/stdout.
	if config.ConsolePipePath != "" {
		go relayConsole(ctx, config.ConsolePipePath)
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
func relayConsole(ctx context.Context, pipePath string) {
	conn, err := connectToConsolePipe(ctx, pipePath)
	if err != nil {
		log.WithError(err).Warn("Could not connect to console pipe — VM output will not be visible")
		return
	}
	defer conn.Close()

	log.WithField("pipe", pipePath).Info("Console pipe connected, relaying I/O")

	// Console output → host stdout.
	go func() {
		if _, err := io.Copy(os.Stdout, conn); err != nil && ctx.Err() == nil {
			log.WithError(err).Debug("Console output relay stopped")
		}
	}()

	// Host stdin → console input (blocks until stdin closes or context cancels).
	go func() {
		if _, err := io.Copy(conn, os.Stdin); err != nil && ctx.Err() == nil {
			log.WithError(err).Debug("Console input relay stopped")
		}
	}()

	// Keep alive until the context is done.
	<-ctx.Done()
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
