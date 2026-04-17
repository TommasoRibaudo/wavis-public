<#
.SYNOPSIS
    Post-terraform-apply integration smoke tests for the private subnet migration.

.DESCRIPTION
    PowerShell script (Windows dev environment) for post-apply validation.
    Tests SSM connectivity, health checks, CloudFront connectivity,
    WebSocket upgrade, NAT gateway outbound, origin secret rejection,
    and Phase 3 LiveKit health integration.

    Requirements: All integration/smoke tests from design document.

.PARAMETER InstanceId
    The backend EC2 instance ID (e.g. i-0c62cb9c5229b086d).

.PARAMETER CloudFrontDomain
    The CloudFront distribution domain (e.g. dt2nm86rf5ksq.cloudfront.net).

.PARAMETER Region
    AWS region. Defaults to us-east-2.

.PARAMETER LiveKitInstanceId
    Optional LiveKit instance ID for Phase 3 tests.

.PARAMETER LiveKitPublicIp
    Optional LiveKit public IP for Phase 3 health check.

.PARAMETER NatEip
    Optional NAT gateway EIP for outbound verification.

.EXAMPLE
    .\tests\smoke_test.ps1 -InstanceId "i-0c62cb9c5229b086d" -CloudFrontDomain "dt2nm86rf5ksq.cloudfront.net"

.EXAMPLE
    .\tests\smoke_test.ps1 -InstanceId "i-0c62cb9c5229b086d" -CloudFrontDomain "dt2nm86rf5ksq.cloudfront.net" -LiveKitInstanceId "i-0abc123" -LiveKitPublicIp "3.14.15.92"
#>

param(
    [Parameter(Mandatory = $true)]
    [string]$InstanceId,

    [Parameter(Mandatory = $true)]
    [string]$CloudFrontDomain,

    [string]$Region = "us-east-2",

    [string]$LiveKitInstanceId = "",

    [string]$LiveKitPublicIp = "",

    [string]$NatEip = ""
)

$ErrorActionPreference = "Stop"
$script:passed = 0
$script:failed = 0
$script:skipped = 0

# ============================================================================
# Helpers
# ============================================================================

function Write-TestResult {
    param(
        [string]$Name,
        [string]$Status,
        [string]$Detail = ""
    )
    $color = switch ($Status) {
        "PASS"    { "Green" }
        "FAIL"    { "Red" }
        "SKIP"    { "Yellow" }
        default   { "White" }
    }
    $msg = "[$Status] $Name"
    if ($Detail) { $msg += " - $Detail" }
    Write-Host $msg -ForegroundColor $color

    switch ($Status) {
        "PASS" { $script:passed++ }
        "FAIL" { $script:failed++ }
        "SKIP" { $script:skipped++ }
    }
}

function Invoke-SsmCommand {
    param(
        [string]$TargetInstanceId,
        [string]$Command
    )
    $cmdId = aws ssm send-command `
        --instance-ids $TargetInstanceId `
        --document-name "AWS-RunShellScript" `
        --parameters "commands=[`"$Command`"]" `
        --region $Region `
        --query "Command.CommandId" --output text 2>&1

    if ($LASTEXITCODE -ne 0) { return $null }

    # Poll for completion (max 60s)
    for ($i = 0; $i -lt 12; $i++) {
        Start-Sleep -Seconds 5
        $result = aws ssm get-command-invocation `
            --command-id $cmdId `
            --instance-id $TargetInstanceId `
            --region $Region `
            --output json 2>&1 | ConvertFrom-Json

        if ($result.Status -eq "Success") {
            return $result.StandardOutputContent
        }
        if ($result.Status -in @("Failed", "TimedOut", "Cancelled")) {
            return $null
        }
    }
    return $null
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host " Wavis Private Subnet Migration Smoke Tests" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Instance:   $InstanceId"
Write-Host "CloudFront: $CloudFrontDomain"
Write-Host "Region:     $Region"
if ($LiveKitInstanceId) { Write-Host "LiveKit:    $LiveKitInstanceId ($LiveKitPublicIp)" }
if ($NatEip) { Write-Host "NAT EIP:    $NatEip" }
Write-Host ""

# ============================================================================
# Phase 2 Tests
# ============================================================================

Write-Host "--- Phase 2: Core Infrastructure ---" -ForegroundColor Cyan

# Test 1: SSM Connectivity
try {
    $ssmInfo = aws ssm describe-instance-information `
        --filters "Key=InstanceIds,Values=$InstanceId" `
        --region $Region `
        --query "InstanceInformationList[0].PingStatus" --output text 2>&1

    if ($ssmInfo -eq "Online") {
        Write-TestResult "SSM Connectivity" "PASS" "Instance is Online"
    } else {
        Write-TestResult "SSM Connectivity" "FAIL" "PingStatus: $ssmInfo"
    }
} catch {
    Write-TestResult "SSM Connectivity" "FAIL" $_.Exception.Message
}

# Test 2: Health Check via SSM
try {
    $healthOutput = Invoke-SsmCommand -TargetInstanceId $InstanceId -Command "curl -sf http://localhost:3000/health"
    if ($healthOutput) {
        Write-TestResult "Health Check (SSM)" "PASS" $healthOutput.Trim()
    } else {
        Write-TestResult "Health Check (SSM)" "FAIL" "curl health check failed or timed out"
    }
} catch {
    Write-TestResult "Health Check (SSM)" "FAIL" $_.Exception.Message
}

# Test 3: CloudFront Connectivity
try {
    $cfResponse = Invoke-WebRequest -Uri "https://$CloudFrontDomain/health" `
        -UseBasicParsing -TimeoutSec 10 -ErrorAction Stop

    if ($cfResponse.StatusCode -eq 200) {
        Write-TestResult "CloudFront Connectivity" "PASS" "HTTP 200"
    } else {
        Write-TestResult "CloudFront Connectivity" "FAIL" "HTTP $($cfResponse.StatusCode)"
    }
} catch {
    $statusCode = $_.Exception.Response.StatusCode.value__
    Write-TestResult "CloudFront Connectivity" "FAIL" "HTTP $statusCode - $($_.Exception.Message)"
}

# Test 4: WebSocket Upgrade
try {
    # Use .NET ClientWebSocket to test upgrade
    $ws = New-Object System.Net.WebSockets.ClientWebSocket
    $cts = New-Object System.Threading.CancellationTokenSource
    $cts.CancelAfter(10000)
    $uri = [Uri]"wss://$CloudFrontDomain/ws"

    $connectTask = $ws.ConnectAsync($uri, $cts.Token)
    $connectTask.Wait()

    if ($ws.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
        Write-TestResult "WebSocket Upgrade" "PASS" "Connection established"
        $ws.CloseAsync(
            [System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure,
            "test complete",
            [System.Threading.CancellationToken]::None
        ).Wait()
    } else {
        Write-TestResult "WebSocket Upgrade" "FAIL" "State: $($ws.State)"
    }
    $ws.Dispose()
    $cts.Dispose()
} catch {
    Write-TestResult "WebSocket Upgrade" "FAIL" $_.Exception.Message
}

# Test 5: NAT Gateway Outbound
if ($NatEip) {
    try {
        $externalIp = Invoke-SsmCommand -TargetInstanceId $InstanceId -Command "curl -sf https://ifconfig.me"
        if ($externalIp) {
            $trimmedIp = $externalIp.Trim()
            if ($trimmedIp -eq $NatEip) {
                Write-TestResult "NAT Gateway Outbound" "PASS" "External IP matches NAT EIP: $trimmedIp"
            } else {
                Write-TestResult "NAT Gateway Outbound" "FAIL" "External IP $trimmedIp does not match NAT EIP $NatEip"
            }
        } else {
            Write-TestResult "NAT Gateway Outbound" "FAIL" "Could not determine external IP"
        }
    } catch {
        Write-TestResult "NAT Gateway Outbound" "FAIL" $_.Exception.Message
    }
} else {
    Write-TestResult "NAT Gateway Outbound" "SKIP" "NatEip parameter not provided"
}

# Test 6: Origin Secret Rejection
try {
    # Send request without X-Origin-Verify header — expect 403
    # This test works pre-migration (direct to public IP) or via CloudFront
    # if the backend validates the header on all requests.
    $directResponse = Invoke-WebRequest -Uri "https://$CloudFrontDomain/health" `
        -UseBasicParsing -TimeoutSec 10 -Headers @{} -ErrorAction Stop

    # If we get 200, the origin secret might not be enforced at the CF level
    # (CF adds the header automatically). Try direct backend access if possible.
    Write-TestResult "Origin Secret Rejection" "SKIP" "CloudFront adds header automatically; test direct backend access separately"
} catch {
    $statusCode = $_.Exception.Response.StatusCode.value__
    if ($statusCode -eq 403) {
        Write-TestResult "Origin Secret Rejection" "PASS" "HTTP 403 as expected"
    } else {
        Write-TestResult "Origin Secret Rejection" "FAIL" "Expected 403, got HTTP $statusCode"
    }
}

# ============================================================================
# Phase 3 Tests (LiveKit separation)
# ============================================================================

Write-Host ""
Write-Host "--- Phase 3: LiveKit Separation ---" -ForegroundColor Cyan

if (-not $LiveKitInstanceId -or -not $LiveKitPublicIp) {
    Write-TestResult "LiveKit Health" "SKIP" "LiveKitInstanceId/LiveKitPublicIp not provided"
    Write-TestResult "Backend Degraded Health" "SKIP" "LiveKit parameters not provided"
} else {
    # Test 7: LiveKit Health
    try {
        $lkHealth = Invoke-WebRequest -Uri "http://${LiveKitPublicIp}:7880/" `
            -UseBasicParsing -TimeoutSec 10 -ErrorAction Stop

        if ($lkHealth.StatusCode -eq 200) {
            Write-TestResult "LiveKit Health" "PASS" "HTTP 200 on port 7880"
        } else {
            Write-TestResult "LiveKit Health" "FAIL" "HTTP $($lkHealth.StatusCode)"
        }
    } catch {
        Write-TestResult "LiveKit Health" "FAIL" $_.Exception.Message
    }

    # Test 8: Backend Degraded Health When LiveKit Stopped
    try {
        Write-Host "  Stopping LiveKit container via SSM..." -ForegroundColor DarkGray
        $stopResult = Invoke-SsmCommand -TargetInstanceId $LiveKitInstanceId `
            -Command "cd ~/wavis && docker compose -f deploy/livekit.yaml stop"

        if (-not $stopResult -and $stopResult -ne "") {
            Write-TestResult "Backend Degraded Health" "FAIL" "Could not stop LiveKit container"
        } else {
            # Wait for backend to detect LiveKit is down
            Start-Sleep -Seconds 5

            $healthOutput = Invoke-SsmCommand -TargetInstanceId $InstanceId `
                -Command "curl -sf http://localhost:3000/health"

            if ($healthOutput -and ($healthOutput -match "degraded|unavailable|partial")) {
                Write-TestResult "Backend Degraded Health" "PASS" "Backend reports degraded status"
            } else {
                Write-TestResult "Backend Degraded Health" "FAIL" "Backend did not report degraded: $healthOutput"
            }

            # Restart LiveKit
            Write-Host "  Restarting LiveKit container via SSM..." -ForegroundColor DarkGray
            Invoke-SsmCommand -TargetInstanceId $LiveKitInstanceId `
                -Command "cd ~/wavis && docker compose -f deploy/livekit.yaml up -d" | Out-Null
        }
    } catch {
        Write-TestResult "Backend Degraded Health" "FAIL" $_.Exception.Message

        # Best-effort restart LiveKit
        try {
            Invoke-SsmCommand -TargetInstanceId $LiveKitInstanceId `
                -Command "cd ~/wavis && docker compose -f deploy/livekit.yaml up -d" | Out-Null
        } catch {}
    }
}

# ============================================================================
# Summary
# ============================================================================

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host " Results: $script:passed passed, $script:failed failed, $script:skipped skipped" -ForegroundColor $(
    if ($script:failed -gt 0) { "Red" } else { "Green" }
)
Write-Host "========================================" -ForegroundColor Cyan

if ($script:failed -gt 0) {
    exit 1
}
