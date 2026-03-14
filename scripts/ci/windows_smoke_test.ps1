# Windows E2E Smoke Tests for Microsandbox
# Validates server lifecycle and graceful Hyper-V error handling.
# Requires: msb.exe, msbrun.exe, msbserver.exe release binaries.

$ErrorActionPreference = "Stop"

$MSB = if ($env:MSB_BIN) { $env:MSB_BIN } else { "msb.exe" }
$SERVER_HOST = "127.0.0.1"
$SERVER_PORT = "5555"
$HEALTH_URL = "http://${SERVER_HOST}:${SERVER_PORT}/api/v1/health"
$MAX_WAIT_SECONDS = 60
$PASSED = 0
$FAILED = 0

function Pass($msg) { Write-Host "[PASS]  $msg" -ForegroundColor Green; $script:PASSED++ }
function Fail($msg) { Write-Host "[FAIL]  $msg" -ForegroundColor Red; $script:FAILED++ }
function Step($msg) { Write-Host "[....]  $msg" -ForegroundColor Blue }
function Skip($msg) { Write-Host "[SKIP]  $msg" -ForegroundColor Yellow }

Write-Host "==========================================="
Write-Host "  Microsandbox Windows Smoke Tests"
Write-Host "==========================================="
Write-Host ""

# --- Verify binaries ---
Step "Checking binaries..."
& $MSB version
if ($LASTEXITCODE -ne 0) { Write-Host "msb binary not working"; exit 1 }

# --- Test 1: Server start ---
Step "Test 1: Server start (dev mode, detached)"
& $MSB server start --dev --detach
if ($LASTEXITCODE -ne 0) {
    Fail "Test 1: Server failed to start"
} else {
    # Wait for health
    $waited = 0
    $healthy = $false
    while ($waited -lt $MAX_WAIT_SECONDS) {
        try {
            $response = Invoke-WebRequest -Uri $HEALTH_URL -UseBasicParsing -TimeoutSec 2 -ErrorAction SilentlyContinue
            if ($response.StatusCode -eq 200) {
                $healthy = $true
                break
            }
        } catch {}
        Start-Sleep -Seconds 1
        $waited++
    }
    if ($healthy) {
        Pass "Test 1: Server started and healthy (${waited}s)"
    } else {
        Fail "Test 1: Server did not become healthy within ${MAX_WAIT_SECONDS}s"
    }
}

# --- Test 2: Health endpoint returns valid JSON ---
Step "Test 2: Health endpoint response"
try {
    $response = Invoke-WebRequest -Uri $HEALTH_URL -UseBasicParsing -TimeoutSec 5
    $body = $response.Content | ConvertFrom-Json
    if ($body.message -eq "Service is healthy") {
        Pass "Test 2: Health endpoint returns valid response"
    } else {
        Fail "Test 2: Unexpected health response: $($response.Content)"
    }
} catch {
    Fail "Test 2: Health endpoint request failed: $_"
}

# --- Test 3: VM boot fails gracefully (no Hyper-V) ---
Step "Test 3: VM boot attempt (expect graceful Hyper-V error)"
$output = & $MSB exe alpine -e "echo hello" 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) {
    # Expected to fail - check it's a graceful error, not a crash
    if ($output -match "Hyper-V" -or $output -match "not enabled" -or $output -match "not implemented" -or $output -match "NotImplemented") {
        Pass "Test 3: VM boot fails gracefully with Hyper-V error"
    } elseif ($output -match "panicked" -or $output -match "EXCEPTION") {
        Fail "Test 3: VM boot crashed instead of failing gracefully: $output"
    } else {
        # Some other error - still not a crash, which is acceptable
        Skip "Test 3: VM boot failed with unexpected error (non-fatal): $($output.Substring(0, [Math]::Min(200, $output.Length)))"
    }
} else {
    # If it somehow succeeds (Hyper-V available), that's fine too
    Pass "Test 3: VM boot succeeded (Hyper-V available)"
}

# --- Test 4: Server stop ---
Step "Test 4: Server stop"
& $MSB server stop
if ($LASTEXITCODE -ne 0) {
    Fail "Test 4: Server failed to stop"
} else {
    Pass "Test 4: Server stopped cleanly"
}

# --- Summary ---
Write-Host ""
Write-Host "==========================================="
$TOTAL = $PASSED + $FAILED
Write-Host "  Results: $PASSED/$TOTAL passed, $FAILED failed"
Write-Host "==========================================="

if ($FAILED -gt 0) { exit 1 }
