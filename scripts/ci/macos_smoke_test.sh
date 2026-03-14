#!/bin/bash
set -euo pipefail

# macOS Smoke Tests for Microsandbox
# Validates server lifecycle on macOS. VM boot is not possible on CI
# (no nested virtualization on GitHub Actions macOS ARM64 runners).
# Requires: msb, msbrun, msbserver release binaries with libkrun.

MSB="${MSB_BIN:-msb}"
SERVER_HOST="127.0.0.1"
SERVER_PORT="5555"
HEALTH_URL="http://${SERVER_HOST}:${SERVER_PORT}/api/v1/health"
MAX_WAIT_SECONDS=60
PASSED=0
FAILED=0

pass() { PASSED=$((PASSED + 1)); printf "\033[1;32m[PASS]\033[0m  %s\n" "$1"; }
fail() { FAILED=$((FAILED + 1)); printf "\033[1;31m[FAIL]\033[0m  %s\n" "$1"; }
step() { printf "\033[1;34m[....]\033[0m  %s\n" "$1"; }
skip() { printf "\033[1;33m[SKIP]\033[0m  %s\n" "$1"; }

cleanup() {
    step "Cleaning up..."
    "$MSB" server stop 2>/dev/null || true
}
trap cleanup EXIT

echo "==========================================="
echo "  Microsandbox macOS Smoke Tests"
echo "==========================================="
echo ""

# --- Verify binaries ---
step "Checking binaries..."
command -v "$MSB" >/dev/null 2>&1 || { fail "msb binary not found"; exit 1; }
"$MSB" version

# --- Test 1: Server start ---
step "Test 1: Server start (dev mode, detached)"
if "$MSB" server start --dev --detach 2>&1; then
    waited=0
    healthy=false
    while [ "$waited" -lt "$MAX_WAIT_SECONDS" ]; do
        if curl -sf "$HEALTH_URL" > /dev/null 2>&1; then
            healthy=true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [ "$healthy" = true ]; then
        pass "Test 1: Server started and healthy (${waited}s)"
    else
        fail "Test 1: Server did not become healthy within ${MAX_WAIT_SECONDS}s"
    fi
else
    fail "Test 1: Server failed to start"
fi

# --- Test 2: Health endpoint ---
step "Test 2: Health endpoint response"
HEALTH_RESPONSE=$(curl -sf "$HEALTH_URL" 2>/dev/null || echo "")
if echo "$HEALTH_RESPONSE" | grep -qF "Service is healthy"; then
    pass "Test 2: Health endpoint returns valid response"
else
    fail "Test 2: Unexpected health response: $HEALTH_RESPONSE"
fi

# --- Test 3: VM boot attempt (expect failure — no nested virt on CI) ---
step "Test 3: VM boot attempt (expect graceful error)"
OUTPUT=$("$MSB" exe alpine -e "echo hello" 2>&1) && RC=$? || RC=$?
if [ "$RC" -ne 0 ]; then
    if echo "$OUTPUT" | grep -qiE "kvm|hypervisor|virtualization|not supported|Error creating"; then
        pass "Test 3: VM boot fails gracefully (no hypervisor access)"
    elif echo "$OUTPUT" | grep -qF "panicked"; then
        fail "Test 3: VM boot crashed: $OUTPUT"
    else
        skip "Test 3: VM boot failed with unexpected error (non-fatal)"
    fi
else
    pass "Test 3: VM boot succeeded (hypervisor available)"
fi

# --- Test 4: Server stop ---
step "Test 4: Server stop"
if "$MSB" server stop 2>&1; then
    pass "Test 4: Server stopped cleanly"
else
    fail "Test 4: Server failed to stop"
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
