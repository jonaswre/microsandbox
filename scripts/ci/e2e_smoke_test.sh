#!/bin/bash
set -euo pipefail

# E2E Smoke Tests for Microsandbox
# Boots real VMs via libkrun and verifies basic sandbox functionality.
# Requires: /dev/kvm, msb/msbrun/msbserver binaries, libkrun libraries.

MSB="${MSB_BIN:-msb}"
SERVER_HOST="127.0.0.1"
SERVER_PORT="5555"
HEALTH_URL="http://${SERVER_HOST}:${SERVER_PORT}/api/v1/health"
MAX_WAIT_SECONDS=60
TEST_IMAGE="alpine"
PASSED=0
FAILED=0

info()  { printf "\033[1;32m[PASS]\033[0m  %s\n" "$1"; }
warn()  { printf "\033[1;33m[SKIP]\033[0m  %s\n" "$1"; }
error() { printf "\033[1;31m[FAIL]\033[0m  %s\n" "$1"; }
step()  { printf "\033[1;34m[....]\033[0m  %s\n" "$1"; }

pass() { PASSED=$((PASSED + 1)); info "$1"; }
fail() { FAILED=$((FAILED + 1)); error "$1"; }

cleanup() {
    step "Cleaning up..."
    "$MSB" server stop 2>/dev/null || true
}
trap cleanup EXIT

# --- Pre-flight ---
echo "==========================================="
echo "  Microsandbox E2E Smoke Tests"
echo "==========================================="

if [ ! -e /dev/kvm ]; then
    error "/dev/kvm not found - cannot run VM boot tests"
    exit 1
fi
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    step "KVM not accessible, attempting chmod..."
    sudo chmod 666 /dev/kvm 2>/dev/null || true
    if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
        error "/dev/kvm is not accessible (check permissions)"
        exit 1
    fi
fi
step "KVM is available"

command -v "$MSB" >/dev/null 2>&1 || { error "msb binary not found"; exit 1; }
"$MSB" version
echo ""

# --- Start Server ---
step "Starting server (dev mode, detached)..."
"$MSB" server start --dev --detach

step "Waiting for server health..."
waited=0
until curl -sf "$HEALTH_URL" > /dev/null 2>&1; do
    sleep 1
    waited=$((waited + 1))
    if [ "$waited" -ge "$MAX_WAIT_SECONDS" ]; then
        error "Server did not become healthy within ${MAX_WAIT_SECONDS}s"
        exit 1
    fi
done
step "Server healthy (${waited}s)"
echo ""

# --- Test 1: Basic exec ---
step "Test 1: Boot VM and run echo"
EXPECTED="hello-from-microsandbox"
if OUTPUT=$("$MSB" exe "$TEST_IMAGE" -e "echo $EXPECTED" 2>&1); then
    if echo "$OUTPUT" | grep -qF "$EXPECTED"; then
        pass "Test 1: Got expected output"
    else
        fail "Test 1: Unexpected output: $OUTPUT"
    fi
else
    fail "Test 1: msb exe failed: $OUTPUT"
fi

# --- Test 2: Verify guest kernel ---
step "Test 2: Verify guest reports Linux kernel"
if OUTPUT=$("$MSB" exe "$TEST_IMAGE" -e "uname -s" 2>&1); then
    if echo "$OUTPUT" | grep -qF "Linux"; then
        pass "Test 2: Guest kernel is Linux"
    else
        fail "Test 2: Expected Linux, got: $OUTPUT"
    fi
else
    fail "Test 2: msb exe failed: $OUTPUT"
fi

# --- Test 3: File I/O inside sandbox ---
step "Test 3: File write/read inside sandbox"
if OUTPUT=$("$MSB" exe "$TEST_IMAGE" -e "echo testdata > /tmp/e2e.txt && cat /tmp/e2e.txt" 2>&1); then
    if echo "$OUTPUT" | grep -qF "testdata"; then
        pass "Test 3: File I/O works"
    else
        fail "Test 3: Expected testdata, got: $OUTPUT"
    fi
else
    fail "Test 3: msb exe failed: $OUTPUT"
fi

# --- Test 4: Network connectivity from inside sandbox ---
step "Test 4: Network access from sandbox"
if OUTPUT=$("$MSB" exe "$TEST_IMAGE" --scope public -e "wget -qO- --timeout=5 http://example.com 2>/dev/null | head -1" 2>&1); then
    if echo "$OUTPUT" | grep -qiF "html"; then
        pass "Test 4: Outbound network works"
    else
        warn "Test 4: Could not verify network (non-fatal)"
    fi
else
    warn "Test 4: Network test skipped (non-fatal)"
fi

# --- Test 5: Environment variables ---
step "Test 5: Environment variable injection"
if OUTPUT=$("$MSB" exe "$TEST_IMAGE" --env "E2E_VAR=smoke_test_42" -e "printenv E2E_VAR" 2>&1); then
    if echo "$OUTPUT" | grep -qF "smoke_test_42"; then
        pass "Test 5: Env var injection works"
    else
        fail "Test 5: Expected smoke_test_42, got: $OUTPUT"
    fi
else
    fail "Test 5: msb exe failed: $OUTPUT"
fi

# --- Summary ---
echo ""
echo "==========================================="
TOTAL=$((PASSED + FAILED))
echo "  Results: $PASSED/$TOTAL passed, $FAILED failed"
echo "==========================================="

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
