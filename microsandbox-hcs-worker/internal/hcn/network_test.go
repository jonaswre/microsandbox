package hcn

import (
	"encoding/json"
	"testing"

	"github.com/Microsoft/go-winio/pkg/guid"
)

func TestMicrosandboxNATGUID_Stable(t *testing.T) {
	// Verify the deterministic GUID produces a consistent string representation.
	expected := "b16b00b5-face-4dad-baad-f00dcafebeef"
	got := MicrosandboxNATGUID.String()
	if got != expected {
		t.Errorf("expected %q, got %q", expected, got)
	}
}

func TestTryOpenNetwork_NonExistent(t *testing.T) {
	// A random GUID should not match any existing network.
	// This test works without admin privileges — HcnOpenNetwork simply fails.
	randomGUID := guid.GUID{
		Data1: 0xdeadbeef, Data2: 0x1234, Data3: 0x5678,
		Data4: [8]byte{0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78},
	}
	if tryOpenNetwork(randomGUID) {
		t.Error("expected tryOpenNetwork to return false for a random GUID")
	}
}

func TestDeleteNetwork_NonExistent(t *testing.T) {
	// Deleting a non-existent network should return an error, not panic.
	randomID := "deadbeef-1234-5678-9abc-def012345678"
	err := DeleteNetwork(randomID)
	if err == nil {
		t.Error("expected error when deleting a non-existent network")
	}
}

func TestFindNetworkByName_NoMatch(t *testing.T) {
	// Searching for a network name that doesn't exist should return empty string.
	id := findNetworkByName("nonexistent-network-12345")
	if id != "" {
		t.Errorf("expected empty string for nonexistent network, got %q", id)
	}
}

func TestBuildNetworkConfig(t *testing.T) {
	cfg := BuildNetworkConfig("test-net", "172.28.0.0/16", "172.28.176.1")

	// Verify JSON structure matches HCN schema expectations.
	data, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("failed to marshal config: %v", err)
	}

	// Unmarshal into a generic map to verify structure.
	var m map[string]interface{}
	if err := json.Unmarshal(data, &m); err != nil {
		t.Fatalf("failed to unmarshal config: %v", err)
	}

	if m["Name"] != "test-net" {
		t.Errorf("expected Name %q, got %v", "test-net", m["Name"])
	}
	if m["Type"] != "NAT" {
		t.Errorf("expected Type %q, got %v", "NAT", m["Type"])
	}

	// Verify schema version.
	sv, ok := m["SchemaVersion"].(map[string]interface{})
	if !ok {
		t.Fatal("SchemaVersion not found or wrong type")
	}
	if sv["Major"] != float64(2) || sv["Minor"] != float64(0) {
		t.Errorf("expected SchemaVersion {2,0}, got %v", sv)
	}

	// Verify IPAM structure.
	ipams, ok := m["Ipams"].([]interface{})
	if !ok || len(ipams) != 1 {
		t.Fatalf("expected 1 IPAM entry, got %v", m["Ipams"])
	}
	ipam := ipams[0].(map[string]interface{})
	subnets := ipam["Subnets"].([]interface{})
	if len(subnets) != 1 {
		t.Fatalf("expected 1 subnet, got %d", len(subnets))
	}
	subnet := subnets[0].(map[string]interface{})
	if subnet["IpAddressPrefix"] != "172.28.0.0/16" {
		t.Errorf("expected subnet 172.28.0.0/16, got %v", subnet["IpAddressPrefix"])
	}
	routes := subnet["Routes"].([]interface{})
	if len(routes) != 1 {
		t.Fatalf("expected 1 route, got %d", len(routes))
	}
	route := routes[0].(map[string]interface{})
	if route["NextHop"] != "172.28.176.1" {
		t.Errorf("expected NextHop 172.28.176.1, got %v", route["NextHop"])
	}
	if route["DestinationPrefix"] != "0.0.0.0/0" {
		t.Errorf("expected DestinationPrefix 0.0.0.0/0, got %v", route["DestinationPrefix"])
	}
}
