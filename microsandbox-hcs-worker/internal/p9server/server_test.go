package p9server

import (
	"testing"

	"github.com/nicholasgasior/microsandbox/microsandbox-hcs-worker/internal/hcs"
)

func TestPortToServiceGUID(t *testing.T) {
	g := hcs.PortToServiceGUID(50000)
	expected := "0000c350-facb-11e6-bd58-64006a7986d3"
	if g.String() != expected {
		t.Errorf("expected %s, got %s", expected, g.String())
	}
}

func TestPortToServiceGUID_Zero(t *testing.T) {
	g := hcs.PortToServiceGUID(0)
	expected := "00000000-facb-11e6-bd58-64006a7986d3"
	if g.String() != expected {
		t.Errorf("expected %s, got %s", expected, g.String())
	}
}

func TestPortToServiceGUID_MaxPort(t *testing.T) {
	g := hcs.PortToServiceGUID(0xFFFFFFFF)
	expected := "ffffffff-facb-11e6-bd58-64006a7986d3"
	if g.String() != expected {
		t.Errorf("expected %s, got %s", expected, g.String())
	}
}

func TestBaseMountPort(t *testing.T) {
	if hcs.BaseMountPort != 50000 {
		t.Errorf("expected BaseMountPort=50000, got %d", hcs.BaseMountPort)
	}
}
