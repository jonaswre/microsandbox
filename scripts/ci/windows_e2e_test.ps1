# Windows E2E Tests for Microsandbox
# Extends the smoke test with full VM lifecycle tests.
# Requires: msb.exe with boot bundle installed, Hyper-V enabled, admin privileges.

$ErrorActionPreference = "Continue"

$MSB = if ($env:MSB_BIN) { $env:MSB_BIN } else { "msb.exe" }
$PASSED = 0
$FAILED = 0
$SKIPPED = 0

function Pass($msg) { Write-Host "[PASS]  $msg" -ForegroundColor Green; $script:PASSED++ }
function Fail($msg) { Write-Host "[FAIL]  $msg" -ForegroundColor Red; $script:FAILED++ }
function Skip($msg) { Write-Host "[SKIP]  $msg" -ForegroundColor Yellow; $script:SKIPPED++ }
function Step($msg) { Write-Host "[....]  $msg" -ForegroundColor Blue }

Write-Host "==========================================="
Write-Host "  Microsandbox Windows E2E Tests"
Write-Host "==========================================="
Write-Host ""

# --- Check prerequisites ---
Step "Checking prerequisites..."
& $MSB version
if ($LASTEXITCODE -ne 0) {
    Write-Host "msb binary not working" -ForegroundColor Red
    exit 1
}

# Check Hyper-V
$hyperv = (Get-CimInstance -ClassName Win32_ComputerSystem).HypervisorPresent
if (-not $hyperv) {
    Write-Host "Hyper-V not available. Skipping VM tests." -ForegroundColor Yellow
    exit 0
}

# Check admin
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "Not running as administrator. HCS requires elevation." -ForegroundColor Yellow
    exit 0
}

# --- Test 1: Basic command execution ---
Step "Test 1: msb exe alpine -e '/bin/echo hello'"
$output = & $MSB exe alpine -e "/bin/echo hello" 2>&1 | Out-String
if ($LASTEXITCODE -eq 0 -and $output -match "hello") {
    Pass "Test 1: Basic command execution"
} elseif ($output -match "Boot bundle") {
    Skip "Test 1: Boot bundle not installed"
} else {
    Fail "Test 1: Expected 'hello' in output, got: $($output.Substring(0, [Math]::Min(200, $output.Length)))"
}

# --- Test 2: Command with arguments ---
Step "Test 2: msb exe alpine -e '/bin/ls /'"
$output = & $MSB exe alpine -e "/bin/ls /" 2>&1 | Out-String
if ($LASTEXITCODE -eq 0 -and $output -match "bin") {
    Pass "Test 2: Command with arguments"
} elseif ($output -match "Boot bundle") {
    Skip "Test 2: Boot bundle not installed"
} else {
    Fail "Test 2: Expected directory listing, got: $($output.Substring(0, [Math]::Min(200, $output.Length)))"
}

# --- Test 3: Named sandbox via microsandbox.yaml ---
Step "Test 3: Named sandbox via config file"
$tempDir = Join-Path $env:TEMP "msb-e2e-$(Get-Random)"
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$configPath = Join-Path $tempDir "microsandbox.yaml"

@"
sandboxes:
  mysandbox:
    image: alpine
    command: /bin/echo
    args:
      - "hello from config"
"@ | Set-Content -Path $configPath -Encoding UTF8

Push-Location $tempDir
try {
    $output = & $MSB run mysandbox 2>&1 | Out-String
    if ($LASTEXITCODE -eq 0 -and $output -match "hello from config") {
        Pass "Test 3: Named sandbox via config"
    } elseif ($output -match "Boot bundle") {
        Skip "Test 3: Boot bundle not installed"
    } else {
        Fail "Test 3: Expected 'hello from config', got: $($output.Substring(0, [Math]::Min(200, $output.Length)))"
    }
} finally {
    Pop-Location
    Remove-Item $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}

# --- Test 4: Back-to-back invocations ---
Step "Test 4: Back-to-back invocations"
$output1 = & $MSB exe alpine -e "/bin/echo run1" 2>&1 | Out-String
$exit1 = $LASTEXITCODE
$output2 = & $MSB exe alpine -e "/bin/echo run2" 2>&1 | Out-String
$exit2 = $LASTEXITCODE

if ($exit1 -eq 0 -and $exit2 -eq 0 -and $output1 -match "run1" -and $output2 -match "run2") {
    Pass "Test 4: Back-to-back invocations"
} elseif ($output1 -match "Boot bundle" -or $output2 -match "Boot bundle") {
    Skip "Test 4: Boot bundle not installed"
} else {
    Fail "Test 4: Back-to-back failed (exit1=$exit1, exit2=$exit2)"
}

# --- Test 5: Volume mount test ---
Step "Test 5: Volume mount (-v C:\temp:/workspace)"
$mountDir = Join-Path $env:TEMP "msb-mount-$(Get-Random)"
New-Item -ItemType Directory -Path $mountDir -Force | Out-Null
"test-content" | Set-Content (Join-Path $mountDir "testfile.txt") -Encoding UTF8

$output = & $MSB exe alpine -v "${mountDir}:/workspace" -e "/bin/cat /workspace/testfile.txt" 2>&1 | Out-String
if ($LASTEXITCODE -eq 0 -and $output -match "test-content") {
    Pass "Test 5: Volume mount works"
} elseif ($output -match "Boot bundle") {
    Skip "Test 5: Boot bundle not installed"
} else {
    Fail "Test 5: Volume mount failed: $($output.Substring(0, [Math]::Min(200, $output.Length)))"
}

Remove-Item $mountDir -Recurse -Force -ErrorAction SilentlyContinue

# --- Summary ---
Write-Host ""
Write-Host "==========================================="
$TOTAL = $PASSED + $FAILED + $SKIPPED
Write-Host "  Results: $PASSED passed, $FAILED failed, $SKIPPED skipped (of $TOTAL)"
Write-Host "==========================================="

if ($FAILED -gt 0) { exit 1 }
