// Package portal provides a TCP relay for proxying connections from the host
// to the guest portal (port 4444) over the HCN NAT network.
package portal

import (
	"context"
	"fmt"
	"io"
	"net"
	"sync"

	log "github.com/sirupsen/logrus"
)

// Relay listens on a host TCP port and relays connections to a guest address.
type Relay struct {
	listener  net.Listener
	guestAddr string
	done      chan struct{}
	wg        sync.WaitGroup
}

// New creates a Relay that listens on 127.0.0.1:<hostPort> and forwards
// connections to guestIP:<guestPort>.
func New(hostPort uint16, guestIP string, guestPort uint16) (*Relay, error) {
	addr := fmt.Sprintf("127.0.0.1:%d", hostPort)
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("bind portal relay on %s: %w", addr, err)
	}

	return &Relay{
		listener:  ln,
		guestAddr: fmt.Sprintf("%s:%d", guestIP, guestPort),
		done:      make(chan struct{}),
	}, nil
}

// Start begins accepting connections in background goroutines. Connections
// are relayed until ctx is cancelled or Stop is called.
func (r *Relay) Start(ctx context.Context) {
	r.wg.Add(1)
	go func() {
		defer r.wg.Done()
		for {
			conn, err := r.listener.Accept()
			if err != nil {
				select {
				case <-r.done:
					return
				default:
				}
				if ctx.Err() != nil {
					return
				}
				log.WithError(err).Debug("Portal relay accept error")
				continue
			}
			r.wg.Add(1)
			go func() {
				defer r.wg.Done()
				r.handleConn(ctx, conn)
			}()
		}
	}()
}

// Stop closes the listener and waits for in-flight connections to drain.
func (r *Relay) Stop() {
	close(r.done)
	r.listener.Close()
	r.wg.Wait()
}

// Addr returns the listener's network address (useful for tests).
func (r *Relay) Addr() net.Addr {
	return r.listener.Addr()
}

func (r *Relay) handleConn(ctx context.Context, client net.Conn) {
	defer client.Close()

	guest, err := net.Dial("tcp", r.guestAddr)
	if err != nil {
		log.WithError(err).WithField("guest", r.guestAddr).Debug("Portal relay: failed to connect to guest")
		return
	}
	defer guest.Close()

	done := make(chan struct{}, 2)

	// client → guest: close guest write side when client is done sending.
	go func() {
		io.Copy(guest, client)
		if tc, ok := guest.(*net.TCPConn); ok {
			tc.CloseWrite()
		}
		done <- struct{}{}
	}()

	// guest → client: close client write side when guest is done sending.
	go func() {
		io.Copy(client, guest)
		if tc, ok := client.(*net.TCPConn); ok {
			tc.CloseWrite()
		}
		done <- struct{}{}
	}()

	// Wait for both directions to finish or context cancellation.
	select {
	case <-ctx.Done():
		return
	case <-r.done:
		return
	case <-done:
		// First direction finished; wait for the second.
	}

	select {
	case <-ctx.Done():
	case <-r.done:
	case <-done:
	}
}
