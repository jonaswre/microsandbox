// Package pipe implements a named pipe server for the microsandbox control protocol.
//
// The Rust control plane communicates with the Go worker over a named pipe
// using a JSON-line protocol. Each line is a PipeMessage.
package pipe

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"sync"
	"time"

	"github.com/Microsoft/go-winio"
	log "github.com/sirupsen/logrus"
)

// PipeMessage represents the named pipe protocol messages.
// Must match the Rust PipeMessage enum in supervisor.rs.
type PipeMessage struct {
	Type     string  `json:"type"`
	State    string  `json:"state,omitempty"`
	GuestPID *uint32 `json:"guest_pid,omitempty"`
	Success  *bool   `json:"success,omitempty"`
	Error    *string `json:"error,omitempty"`
	Cols     uint16  `json:"cols,omitempty"`
	Rows     uint16  `json:"rows,omitempty"`
}

// Server manages the named pipe listener and handles control commands.
type Server struct {
	pipePath string
	cancel   context.CancelFunc
	listener net.Listener
	wg       sync.WaitGroup
}

// NewServer creates a new pipe server.
func NewServer(pipePath string, cancel context.CancelFunc) *Server {
	return &Server{
		pipePath: pipePath,
		cancel:   cancel,
	}
}

// ListenAndServe starts listening on the named pipe and handles connections.
// It blocks until the context is cancelled or the listener is closed.
func (s *Server) ListenAndServe(ctx context.Context) error {
	cfg := &winio.PipeConfig{
		SecurityDescriptor: "", // default security
		MessageMode:        false,
		InputBufferSize:    4096,
		OutputBufferSize:   4096,
	}

	// Retry pipe creation — a stale pipe from a prior crashed run may still exist.
	var listener net.Listener
	var err error
	for i := 0; i < 5; i++ {
		listener, err = winio.ListenPipe(s.pipePath, cfg)
		if err == nil {
			break
		}
		time.Sleep(500 * time.Millisecond)
	}
	if err != nil {
		return fmt.Errorf("listen on pipe %q: %w", s.pipePath, err)
	}
	s.listener = listener

	log.WithField("pipe", s.pipePath).Info("Named pipe server listening")

	go func() {
		<-ctx.Done()
		listener.Close()
	}()

	for {
		conn, err := listener.Accept()
		if err != nil {
			select {
			case <-ctx.Done():
				return nil // normal shutdown
			default:
				log.WithError(err).Warn("Accept failed on named pipe")
				continue
			}
		}

		s.wg.Add(1)
		go func() {
			defer s.wg.Done()
			s.handleConn(conn)
		}()
	}
}

// Stop gracefully shuts down the pipe server.
func (s *Server) Stop() {
	if s.listener != nil {
		s.listener.Close()
	}
	// Wait for all active connections to finish (with a timeout).
	done := make(chan struct{})
	go func() {
		s.wg.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		log.Warn("Timed out waiting for pipe connections to close")
	}
}

// handleConn processes a single named pipe connection.
// Protocol: each line is a JSON PipeMessage; responses are written back as JSON lines.
func (s *Server) handleConn(conn net.Conn) {
	defer conn.Close()

	scanner := bufio.NewScanner(conn)
	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}

		var msg PipeMessage
		if err := json.Unmarshal(line, &msg); err != nil {
			log.WithError(err).Warn("Failed to parse pipe message")
			continue
		}

		response := s.handleMessage(&msg)
		if response != nil {
			respBytes, err := json.Marshal(response)
			if err != nil {
				log.WithError(err).Error("Failed to marshal pipe response")
				continue
			}
			respBytes = append(respBytes, '\n')
			if _, err := conn.Write(respBytes); err != nil {
				log.WithError(err).Warn("Failed to write pipe response")
				return
			}
		}
	}
}

// handleMessage dispatches a protocol message and returns a response (or nil).
func (s *Server) handleMessage(msg *PipeMessage) *PipeMessage {
	switch msg.Type {
	case "status":
		return &PipeMessage{
			Type:  "status_response",
			State: "running",
		}

	case "stop":
		log.Info("Received stop command via named pipe")
		success := true

		// Signal shutdown — the main loop will handle HCS teardown.
		s.cancel()

		return &PipeMessage{
			Type:    "stop_ack",
			Success: &success,
		}

	case "attach":
		log.WithFields(log.Fields{
			"cols": msg.Cols,
			"rows": msg.Rows,
		}).Info("Received attach request (not yet implemented)")
		return nil

	case "resize":
		log.WithFields(log.Fields{
			"cols": msg.Cols,
			"rows": msg.Rows,
		}).Debug("Received resize event (not yet implemented)")
		return nil

	default:
		log.WithField("type", msg.Type).Warn("Unknown pipe message type")
		return nil
	}
}
