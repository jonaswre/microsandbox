// Package hcn provides bindings to the Windows Host Compute Network (HCN) API
// via computenetwork.dll for managing NAT networks, endpoints, and load balancers.
package hcn

import (
	"encoding/json"
	"fmt"
	"syscall"
	"unsafe"

	"github.com/Microsoft/go-winio/pkg/guid"
	log "github.com/sirupsen/logrus"
	"golang.org/x/sys/windows"
)

//--------------------------------------------------------------------------------------------------
// Win32 HCN API bindings (computenetwork.dll)
//--------------------------------------------------------------------------------------------------

var (
	modComputeNetwork = windows.NewLazySystemDLL("computenetwork.dll")

	procHcnEnumerateNetworks    = modComputeNetwork.NewProc("HcnEnumerateNetworks")
	procHcnCreateNetwork        = modComputeNetwork.NewProc("HcnCreateNetwork")
	procHcnDeleteNetwork        = modComputeNetwork.NewProc("HcnDeleteNetwork")
	procHcnEnumerateEndpoints   = modComputeNetwork.NewProc("HcnEnumerateEndpoints")
	procHcnCreateEndpoint       = modComputeNetwork.NewProc("HcnCreateEndpoint")
	procHcnDeleteEndpoint       = modComputeNetwork.NewProc("HcnDeleteEndpoint")
	procHcnEnumerateLoadBalancers = modComputeNetwork.NewProc("HcnEnumerateLoadBalancers")
	procHcnCreateLoadBalancer   = modComputeNetwork.NewProc("HcnCreateLoadBalancer")
	procHcnDeleteLoadBalancer   = modComputeNetwork.NewProc("HcnDeleteLoadBalancer")
	procHcnCloseNetwork         = modComputeNetwork.NewProc("HcnCloseNetwork")
	procHcnCloseEndpoint        = modComputeNetwork.NewProc("HcnCloseEndpoint")
	procHcnCloseLoadBalancer    = modComputeNetwork.NewProc("HcnCloseLoadBalancer")
)

//--------------------------------------------------------------------------------------------------
// HCN JSON schema types
//--------------------------------------------------------------------------------------------------

// NetworkConfig is the HCN network configuration for creation.
type NetworkConfig struct {
	SchemaVersion SchemaVersion `json:"SchemaVersion"`
	Name          string        `json:"Name"`
	Type          string        `json:"Type"`
	Ipams         []Ipam        `json:"Ipams,omitempty"`
}

// SchemaVersion for HCN objects.
type SchemaVersion struct {
	Major int `json:"Major"`
	Minor int `json:"Minor"`
}

// Ipam describes IP address management config.
type Ipam struct {
	Type    string   `json:"Type,omitempty"`
	Subnets []Subnet `json:"Subnets,omitempty"`
}

// Subnet describes a network subnet.
type Subnet struct {
	IpAddressPrefix string  `json:"IpAddressPrefix"`
	Routes          []Route `json:"Routes,omitempty"`
}

// Route describes a network route.
type Route struct {
	NextHop           string `json:"NextHop"`
	DestinationPrefix string `json:"DestinationPrefix"`
}

// EndpointConfig is the HCN endpoint configuration.
type EndpointConfig struct {
	SchemaVersion   SchemaVersion `json:"SchemaVersion"`
	Name            string        `json:"Name,omitempty"`
	HostComputeNetwork string     `json:"HostComputeNetwork"`
}

// LoadBalancerConfig is the HCN load balancer configuration.
type LoadBalancerConfig struct {
	SchemaVersion        SchemaVersion    `json:"SchemaVersion"`
	HostComputeEndpoints []string         `json:"HostComputeEndpoints"`
	PortMappings         []PortMapping    `json:"PortMappings"`
}

// PortMapping describes a port forwarding rule.
type PortMapping struct {
	Protocol     uint32 `json:"Protocol"` // 6 = TCP, 17 = UDP
	InternalPort uint16 `json:"InternalPort"`
	ExternalPort uint16 `json:"ExternalPort"`
}

// NetworkResult is a partial HCN network response (for ID extraction).
type NetworkResult struct {
	ID   string `json:"ID"`
	Name string `json:"Name"`
}

// EndpointResult is a partial HCN endpoint response.
type EndpointResult struct {
	ID        string `json:"ID"`
	IPAddress string `json:"IPAddress"`
}

//--------------------------------------------------------------------------------------------------
// Public API
//--------------------------------------------------------------------------------------------------

// EnsureNATNetwork finds or creates a NAT network with the given name/subnet.
func EnsureNATNetwork(name, subnet, gateway string) (string, error) {
	// Enumerate existing networks.
	networks, err := enumerateNetworks()
	if err != nil {
		return "", fmt.Errorf("enumerate networks: %w", err)
	}

	// Check if network already exists.
	for _, n := range networks {
		if n.Name == name {
			log.WithField("id", n.ID).Info("Reusing existing NAT network")
			return n.ID, nil
		}
	}

	// Create new NAT network.
	cfg := NetworkConfig{
		SchemaVersion: SchemaVersion{Major: 2, Minor: 0},
		Name:          name,
		Type:          "NAT",
		Ipams: []Ipam{{
			Subnets: []Subnet{{
				IpAddressPrefix: subnet,
				Routes: []Route{{
					NextHop:           gateway,
					DestinationPrefix: "0.0.0.0/0",
				}},
			}},
		}},
	}

	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return "", err
	}

	id, err := createNetwork(string(cfgJSON))
	if err != nil {
		// If creation failed because the network already exists, try to find it.
		networks2, _ := enumerateNetworks()
		for _, n := range networks2 {
			if n.Name == name {
				log.WithField("id", n.ID).Info("Found existing NAT network after create conflict")
				return n.ID, nil
			}
		}
		return "", fmt.Errorf("create NAT network %q: %w", name, err)
	}

	log.WithFields(log.Fields{"id": id, "name": name}).Info("Created NAT network")
	return id, nil
}

// CreateEndpoint creates an HCN endpoint attached to the given network.
func CreateEndpoint(networkID, name string) (epID string, ipAddr string, err error) {
	cfg := EndpointConfig{
		SchemaVersion:      SchemaVersion{Major: 2, Minor: 0},
		Name:               name,
		HostComputeNetwork: networkID,
	}

	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return "", "", err
	}

	resultJSON, err := createEndpoint(networkID, string(cfgJSON))
	if err != nil {
		return "", "", fmt.Errorf("create endpoint %q: %w", name, err)
	}

	var result EndpointResult
	if err := json.Unmarshal([]byte(resultJSON), &result); err != nil {
		return "", "", fmt.Errorf("parse endpoint result: %w (raw: %s)", err, resultJSON)
	}

	log.WithFields(log.Fields{
		"id": result.ID,
		"ip": result.IPAddress,
	}).Info("Created HCN endpoint")
	return result.ID, result.IPAddress, nil
}

// CreateLoadBalancer creates a TCP load balancer forwarding hostPort to guestPort
// through the specified endpoint.
func CreateLoadBalancer(endpointID string, hostPort, guestPort uint16) (string, error) {
	cfg := LoadBalancerConfig{
		SchemaVersion:        SchemaVersion{Major: 2, Minor: 0},
		HostComputeEndpoints: []string{endpointID},
		PortMappings: []PortMapping{{
			Protocol:     6, // TCP
			InternalPort: guestPort,
			ExternalPort: hostPort,
		}},
	}

	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return "", err
	}

	id, err := createLoadBalancer(string(cfgJSON))
	if err != nil {
		return "", fmt.Errorf("create load balancer %d->%d: %w", hostPort, guestPort, err)
	}

	log.WithFields(log.Fields{
		"id":        id,
		"host_port": hostPort,
		"guest_port": guestPort,
	}).Info("Created HCN load balancer")
	return id, nil
}

// DeleteEndpoint removes an HCN endpoint by ID.
func DeleteEndpoint(id string) error {
	return deleteEndpoint(id)
}

// DeleteLoadBalancer removes an HCN load balancer by ID.
func DeleteLoadBalancer(id string) error {
	return deleteLoadBalancer(id)
}

//--------------------------------------------------------------------------------------------------
// Win32 syscall wrappers
//--------------------------------------------------------------------------------------------------

func enumerateNetworks() ([]NetworkResult, error) {
	query := `{"SchemaVersion":{"Major":2,"Minor":0}}`
	queryUTF16, _ := syscall.UTF16PtrFromString(query)

	var resultBuf *uint16
	var errBuf *uint16
	r1, _, _ := procHcnEnumerateNetworks.Call(
		uintptr(unsafe.Pointer(queryUTF16)),
		uintptr(unsafe.Pointer(&resultBuf)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return nil, hcnError("HcnEnumerateNetworks", r1, errBuf)
	}
	defer coTaskMemFree(resultBuf)

	resultStr := windows.UTF16PtrToString(resultBuf)
	var ids []string
	if err := json.Unmarshal([]byte(resultStr), &ids); err != nil {
		return nil, fmt.Errorf("parse network IDs: %w", err)
	}

	// For each network ID, we need to open it and get the name.
	// For simplicity, we open+query each one.
	var results []NetworkResult
	for _, id := range ids {
		name, err := getNetworkName(id)
		if err != nil {
			log.WithError(err).WithField("id", id).Debug("Could not get network name")
			continue
		}
		results = append(results, NetworkResult{ID: id, Name: name})
	}
	return results, nil
}

func getNetworkName(id string) (string, error) {
	g, err := guid.FromString(id)
	if err != nil {
		return "", fmt.Errorf("parse network GUID %q: %w", id, err)
	}
	var handle uintptr
	var errBuf *uint16

	r1, _, _ := modComputeNetwork.NewProc("HcnOpenNetwork").Call(
		uintptr(unsafe.Pointer(&g)),
		uintptr(unsafe.Pointer(&handle)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnOpenNetwork", r1, errBuf)
	}
	defer procHcnCloseNetwork.Call(handle)

	// Query network properties.
	query := `{"SchemaVersion":{"Major":2,"Minor":0}}`
	queryUTF16, _ := syscall.UTF16PtrFromString(query)
	var propBuf *uint16
	r1, _, _ = modComputeNetwork.NewProc("HcnQueryNetworkProperties").Call(
		handle,
		uintptr(unsafe.Pointer(queryUTF16)),
		uintptr(unsafe.Pointer(&propBuf)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnQueryNetworkProperties", r1, errBuf)
	}
	defer coTaskMemFree(propBuf)

	propStr := windows.UTF16PtrToString(propBuf)
	var props struct {
		Name string `json:"Name"`
	}
	if err := json.Unmarshal([]byte(propStr), &props); err != nil {
		return "", err
	}
	return props.Name, nil
}

func createNetwork(settingsJSON string) (string, error) {
	g, err := guid.NewV4()
	if err != nil {
		return "", fmt.Errorf("generate network GUID: %w", err)
	}
	settingsUTF16, _ := syscall.UTF16PtrFromString(settingsJSON)

	var handle uintptr
	var errBuf *uint16
	r1, _, _ := procHcnCreateNetwork.Call(
		uintptr(unsafe.Pointer(&g)),
		uintptr(unsafe.Pointer(settingsUTF16)),
		uintptr(unsafe.Pointer(&handle)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnCreateNetwork", r1, errBuf)
	}
	procHcnCloseNetwork.Call(handle)
	return g.String(), nil
}

func createEndpoint(networkID, settingsJSON string) (string, error) {
	netGUID, err := guid.FromString(networkID)
	if err != nil {
		return "", fmt.Errorf("parse network GUID %q: %w", networkID, err)
	}
	var netHandle uintptr
	var errBuf *uint16

	r1, _, _ := modComputeNetwork.NewProc("HcnOpenNetwork").Call(
		uintptr(unsafe.Pointer(&netGUID)),
		uintptr(unsafe.Pointer(&netHandle)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnOpenNetwork", r1, errBuf)
	}
	defer procHcnCloseNetwork.Call(netHandle)

	epGUID, err := guid.NewV4()
	if err != nil {
		return "", fmt.Errorf("generate endpoint GUID: %w", err)
	}
	settingsUTF16, _ := syscall.UTF16PtrFromString(settingsJSON)

	var epHandle uintptr
	r1, _, _ = procHcnCreateEndpoint.Call(
		netHandle,
		uintptr(unsafe.Pointer(&epGUID)),
		uintptr(unsafe.Pointer(settingsUTF16)),
		uintptr(unsafe.Pointer(&epHandle)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnCreateEndpoint", r1, errBuf)
	}
	defer procHcnCloseEndpoint.Call(epHandle)

	// Query endpoint properties to get the assigned IP.
	query := `{"SchemaVersion":{"Major":2,"Minor":0}}`
	queryUTF16, _ := syscall.UTF16PtrFromString(query)
	var propBuf *uint16
	r1, _, _ = modComputeNetwork.NewProc("HcnQueryEndpointProperties").Call(
		epHandle,
		uintptr(unsafe.Pointer(queryUTF16)),
		uintptr(unsafe.Pointer(&propBuf)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnQueryEndpointProperties", r1, errBuf)
	}
	defer coTaskMemFree(propBuf)

	return windows.UTF16PtrToString(propBuf), nil
}

func createLoadBalancer(settingsJSON string) (string, error) {
	g, err := guid.NewV4()
	if err != nil {
		return "", fmt.Errorf("generate load balancer GUID: %w", err)
	}
	settingsUTF16, _ := syscall.UTF16PtrFromString(settingsJSON)

	var handle uintptr
	var errBuf *uint16
	r1, _, _ := procHcnCreateLoadBalancer.Call(
		uintptr(unsafe.Pointer(&g)),
		uintptr(unsafe.Pointer(settingsUTF16)),
		uintptr(unsafe.Pointer(&handle)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return "", hcnError("HcnCreateLoadBalancer", r1, errBuf)
	}
	procHcnCloseLoadBalancer.Call(handle)
	return g.String(), nil
}

func deleteEndpoint(id string) error {
	g, err := guid.FromString(id)
	if err != nil {
		return fmt.Errorf("parse endpoint GUID %q: %w", id, err)
	}
	var errBuf *uint16
	r1, _, _ := procHcnDeleteEndpoint.Call(
		uintptr(unsafe.Pointer(&g)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return hcnError("HcnDeleteEndpoint", r1, errBuf)
	}
	return nil
}

func deleteLoadBalancer(id string) error {
	g, err := guid.FromString(id)
	if err != nil {
		return fmt.Errorf("parse load balancer GUID %q: %w", id, err)
	}
	var errBuf *uint16
	r1, _, _ := procHcnDeleteLoadBalancer.Call(
		uintptr(unsafe.Pointer(&g)),
		uintptr(unsafe.Pointer(&errBuf)),
	)
	if r1 != 0 {
		return hcnError("HcnDeleteLoadBalancer", r1, errBuf)
	}
	return nil
}

//--------------------------------------------------------------------------------------------------
// Helpers
//--------------------------------------------------------------------------------------------------

func hcnError(fn string, hr uintptr, errBuf *uint16) error {
	msg := ""
	if errBuf != nil {
		msg = windows.UTF16PtrToString(errBuf)
		coTaskMemFree(errBuf)
	}
	if msg != "" {
		return fmt.Errorf("%s failed (HRESULT 0x%08X): %s", fn, hr, msg)
	}
	return fmt.Errorf("%s failed (HRESULT 0x%08X)", fn, hr)
}

func coTaskMemFree(p *uint16) {
	if p != nil {
		windows.CoTaskMemFree(unsafe.Pointer(p))
	}
}

