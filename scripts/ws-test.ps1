param(
    [string]$Url = "ws://localhost:3000/ws"
)

$ws = New-Object System.Net.WebSockets.ClientWebSocket
$token = [Threading.CancellationToken]::None

Write-Host "Connecting to $Url ..." -ForegroundColor Cyan
try {
    $null = $ws.ConnectAsync([Uri]$Url, $token).GetAwaiter().GetResult()
} catch {
    Write-Host "Failed to connect: $_" -ForegroundColor Red
    exit 1
}
Write-Host "Connected! Type JSON and press Enter. Ctrl+C to quit." -ForegroundColor Green
Write-Host ""

$buf = New-Object byte[] 65536
$seg = New-Object ArraySegment[byte] $buf, 0, $buf.Length
$script:recvTask = $null

function Start-Receive {
    if ($ws.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
        $script:recvTask = $ws.ReceiveAsync($seg, $token)
    }
}

function Poll-Messages {
    while ($null -ne $script:recvTask -and $script:recvTask.IsCompleted) {
        if ($script:recvTask.IsFaulted) {
            Write-Host "`n<< [Receive error]" -ForegroundColor Red
            $script:recvTask = $null
            return
        }
        $result = $script:recvTask.Result
        if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Text) {
            $text = [System.Text.Encoding]::UTF8.GetString($buf, 0, $result.Count)
            try {
                $pretty = $text | ConvertFrom-Json | ConvertTo-Json -Compress
                Write-Host "`n<< $pretty" -ForegroundColor Yellow
            } catch {
                Write-Host "`n<< $text" -ForegroundColor Yellow
            }
        }
        elseif ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
            Write-Host "`n<< [Server closed connection]" -ForegroundColor Red
            $script:recvTask = $null
            return
        }
        if ($ws.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
            Start-Receive
        } else {
            $script:recvTask = $null
        }
    }
}

Start-Receive

try {
    while ($ws.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
        Poll-Messages
        Write-Host ">> " -NoNewline -ForegroundColor Green
        $line = Read-Host

        Poll-Messages

        if ([string]::IsNullOrWhiteSpace($line)) { continue }

        # Normalize smart/curly quotes to straight ASCII quotes
        $line = $line.Replace([char]0x201C, '"').Replace([char]0x201D, '"')
        $line = $line.Replace([char]0x201E, '"').Replace([char]0x201F, '"')
        $line = $line.Replace([char]0x2018, "'").Replace([char]0x2019, "'")

        $bytes = [System.Text.Encoding]::UTF8.GetBytes($line)
        $segment = New-Object ArraySegment[byte] $bytes, 0, $bytes.Length
        try {
            $null = $ws.SendAsync(
                $segment,
                [System.Net.WebSockets.WebSocketMessageType]::Text,
                $true,
                $token
            ).GetAwaiter().GetResult()
        } catch {
            Write-Host "Send error: $_" -ForegroundColor Red
            break
        }

        Start-Sleep -Milliseconds 300
        Poll-Messages
    }
} finally {
    if ($ws.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
        try {
            $null = $ws.CloseAsync(
                [System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure,
                "bye",
                $token
            ).GetAwaiter().GetResult()
        } catch {}
    }
    $ws.Dispose()
    Write-Host "Disconnected." -ForegroundColor Cyan
}
