package pipe

import (
	"encoding/json"
	"testing"
)

func TestPipeMessage_StatusQuery(t *testing.T) {
	msg := PipeMessage{Type: "status"}
	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Type != "status" {
		t.Errorf("expected type 'status', got %q", decoded.Type)
	}
}

func TestPipeMessage_StatusResponse(t *testing.T) {
	pid := uint32(42)
	msg := PipeMessage{
		Type:     "status_response",
		State:    "running",
		GuestPID: &pid,
	}

	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Type != "status_response" {
		t.Errorf("expected type 'status_response', got %q", decoded.Type)
	}
	if decoded.State != "running" {
		t.Errorf("expected state 'running', got %q", decoded.State)
	}
	if decoded.GuestPID == nil || *decoded.GuestPID != 42 {
		t.Error("expected guest_pid 42")
	}
}

func TestPipeMessage_StopCommand(t *testing.T) {
	msg := PipeMessage{Type: "stop"}
	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Type != "stop" {
		t.Errorf("expected type 'stop', got %q", decoded.Type)
	}
}

func TestPipeMessage_StopAck(t *testing.T) {
	success := true
	msg := PipeMessage{
		Type:    "stop_ack",
		Success: &success,
	}

	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Type != "stop_ack" {
		t.Errorf("expected type 'stop_ack', got %q", decoded.Type)
	}
	if decoded.Success == nil || !*decoded.Success {
		t.Error("expected success=true")
	}
	if decoded.Error != nil {
		t.Error("expected no error")
	}
}

func TestPipeMessage_StopAckWithError(t *testing.T) {
	success := false
	errMsg := "shutdown timeout"
	msg := PipeMessage{
		Type:    "stop_ack",
		Success: &success,
		Error:   &errMsg,
	}

	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Success == nil || *decoded.Success {
		t.Error("expected success=false")
	}
	if decoded.Error == nil || *decoded.Error != "shutdown timeout" {
		t.Error("expected error 'shutdown timeout'")
	}
}

func TestPipeMessage_AttachRequest(t *testing.T) {
	msg := PipeMessage{Type: "attach", Cols: 80, Rows: 24}
	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var decoded PipeMessage
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if decoded.Cols != 80 || decoded.Rows != 24 {
		t.Errorf("expected 80x24, got %dx%d", decoded.Cols, decoded.Rows)
	}
}

func TestPipeMessage_MatchesRustFormat(t *testing.T) {
	// Verify that our Go PipeMessage serialization matches what the Rust
	// supervisor.rs PipeMessage expects.

	// Rust StatusQuery: {"type":"status"}
	statusQuery := PipeMessage{Type: "status"}
	data, _ := json.Marshal(statusQuery)
	if string(data) != `{"type":"status"}` {
		t.Errorf("StatusQuery JSON mismatch: %s", string(data))
	}

	// Rust StopCommand: {"type":"stop"}
	stopCmd := PipeMessage{Type: "stop"}
	data, _ = json.Marshal(stopCmd)
	if string(data) != `{"type":"stop"}` {
		t.Errorf("StopCommand JSON mismatch: %s", string(data))
	}
}

func TestHandleMessage_Status(t *testing.T) {
	server := &Server{
		cancel: func() {},
	}

	msg := &PipeMessage{Type: "status"}
	resp := server.handleMessage(msg)

	if resp == nil {
		t.Fatal("expected response for status query")
	}
	if resp.Type != "status_response" {
		t.Errorf("expected type 'status_response', got %q", resp.Type)
	}
	if resp.State != "running" {
		t.Errorf("expected state 'running', got %q", resp.State)
	}
}

func TestHandleMessage_Stop(t *testing.T) {
	cancelled := false
	server := &Server{
		cancel: func() { cancelled = true },
	}

	msg := &PipeMessage{Type: "stop"}
	resp := server.handleMessage(msg)

	if resp == nil {
		t.Fatal("expected response for stop command")
	}
	if resp.Type != "stop_ack" {
		t.Errorf("expected type 'stop_ack', got %q", resp.Type)
	}
	if resp.Success == nil || !*resp.Success {
		t.Error("expected success=true")
	}
	if !cancelled {
		t.Error("expected cancel to be called")
	}
}

func TestHandleMessage_Unknown(t *testing.T) {
	server := &Server{
		cancel: func() {},
	}

	msg := &PipeMessage{Type: "unknown_type"}
	resp := server.handleMessage(msg)

	if resp != nil {
		t.Error("expected nil response for unknown message type")
	}
}
