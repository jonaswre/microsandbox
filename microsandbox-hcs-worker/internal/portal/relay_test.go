package portal

import (
	"context"
	"fmt"
	"io"
	"net"
	"testing"
	"time"
)

func TestRelay_EndToEnd(t *testing.T) {
	// Start a mock "guest portal" TCP server that echoes back data.
	guest, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("failed to start mock guest: %v", err)
	}
	defer guest.Close()

	go func() {
		for {
			conn, err := guest.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				io.Copy(conn, conn)
			}()
		}
	}()

	guestPort := guest.Addr().(*net.TCPAddr).Port

	// Create and start the relay.
	relay, err := New(0, "127.0.0.1", uint16(guestPort))
	if err != nil {
		t.Fatalf("failed to create relay: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	relay.Start(ctx)
	defer relay.Stop()

	// Connect a client through the relay.
	relayAddr := relay.Addr().String()
	conn, err := net.DialTimeout("tcp", relayAddr, 2*time.Second)
	if err != nil {
		t.Fatalf("failed to connect to relay: %v", err)
	}
	defer conn.Close()

	// Send data and verify echo.
	msg := "hello portal"
	fmt.Fprint(conn, msg)
	conn.(*net.TCPConn).CloseWrite()

	buf, err := io.ReadAll(conn)
	if err != nil {
		t.Fatalf("read error: %v", err)
	}
	if string(buf) != msg {
		t.Errorf("expected %q, got %q", msg, string(buf))
	}
}

func TestNew_InvalidPort(t *testing.T) {
	// Port 0 should succeed (OS picks a port).
	relay, err := New(0, "127.0.0.1", 4444)
	if err != nil {
		t.Fatalf("unexpected error with port 0: %v", err)
	}
	relay.Stop()
}

func TestRelay_StopClosesListener(t *testing.T) {
	relay, err := New(0, "127.0.0.1", 4444)
	if err != nil {
		t.Fatalf("failed to create relay: %v", err)
	}

	addr := relay.Addr().String()
	ctx := context.Background()
	relay.Start(ctx)
	relay.Stop()

	// After stop, connecting should fail.
	_, err = net.DialTimeout("tcp", addr, 500*time.Millisecond)
	if err == nil {
		t.Error("expected connection to fail after relay stop")
	}
}
