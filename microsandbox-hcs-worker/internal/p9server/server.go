// Package p9server provides a 9p2000.L file server over Hyper-V sockets (HvSocket)
// for live bidirectional host directory sharing with HCS Linux VMs.
//
// Each Plan9 share gets its own HvSocket listener on a unique service GUID derived
// from the port number. The guest bootstrap connects via AF_VSOCK and mounts using
// the kernel's built-in 9p filesystem with trans=fd.
package p9server

import (
	"fmt"
	"sync"

	winio "github.com/Microsoft/go-winio"
	"github.com/Microsoft/go-winio/pkg/guid"
	log "github.com/sirupsen/logrus"

	"github.com/hugelgupf/p9/fsimpl/localfs"
	"github.com/hugelgupf/p9/p9"

	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcs"
)

// ShareServer manages 9p file servers for host directory sharing over HvSocket.
type ShareServer struct {
	shares    []hcs.Plan9Share
	listeners []*winio.HvsockListener
	wg        sync.WaitGroup
}

// New creates a new ShareServer for the given Plan9 shares.
func New(shares []hcs.Plan9Share) *ShareServer {
	return &ShareServer{shares: shares}
}

// Start begins listening for 9p connections on HvSocket for each share.
// Must be called before the VM starts so the guest bootstrap can connect immediately.
func (s *ShareServer) Start() error {
	for i, share := range s.shares {
		port := hcs.BaseMountPort + uint32(i)
		serviceGUID := hcs.PortToServiceGUID(port)

		addr := &winio.HvsockAddr{
			VMID:      guid.GUID{}, // wildcard - accept from any VM
			ServiceID: serviceGUID,
		}

		listener, err := winio.ListenHvsock(addr)
		if err != nil {
			s.Stop()
			return fmt.Errorf("listen hvsock port %d for %s: %w", port, share.Name, err)
		}

		s.listeners = append(s.listeners, listener)

		hostPath := share.HostPath
		shareName := share.Name
		s.wg.Add(1)
		go func(l *winio.HvsockListener, hp, name string, p uint32) {
			defer s.wg.Done()
			s.serveShare(l, hp, name, p)
		}(listener, hostPath, shareName, port)
	}

	return nil
}

// serveShare accepts connections on the HvSocket listener and serves 9p requests.
// Each connection is served synchronously (one mount = one connection from the VM).
func (s *ShareServer) serveShare(l *winio.HvsockListener, hostPath, name string, port uint32) {
	log.WithFields(log.Fields{
		"port":     port,
		"hostPath": hostPath,
		"name":     name,
	}).Info("9p share server listening")

	attacher := localfs.Attacher(hostPath)
	server := p9.NewServer(attacher)

	for {
		conn, err := l.Accept()
		if err != nil {
			// Listener closed — normal shutdown.
			log.WithField("port", port).Debug("9p listener closed")
			return
		}

		log.WithField("port", port).Info("9p client connected")

		// Handle blocks until the connection closes (VM shutdown or unmount).
		if err := server.Handle(conn, conn); err != nil {
			log.WithError(err).WithField("port", port).Debug("9p connection ended")
		}
	}
}

// Stop closes all listeners and waits for server goroutines to finish.
func (s *ShareServer) Stop() {
	for _, l := range s.listeners {
		_ = l.Close()
	}
	s.wg.Wait()
}
