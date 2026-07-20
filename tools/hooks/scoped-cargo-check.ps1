$ErrorActionPreference = 'Stop'
$stdin = [Console]::In.ReadToEnd()
try { $input_json = $stdin | ConvertFrom-Json } catch { $input_json = $null }

if ($input_json -and $input_json.stop_hook_active -eq $true) { exit 0 }

$repoRoot = $null
if ($input_json -and $input_json.cwd) { $repoRoot = $input_json.cwd }
if (-not $repoRoot) { $repoRoot = (Get-Location).Path }

function Get-ChangedRustCrates {
    param([string]$Root)

    Push-Location $Root
    try {
        $changed = git diff --name-only HEAD -- '*.rs' 2>$null
        $untracked = git ls-files --others --exclude-standard -- '*.rs' 2>$null
        $allFiles = @($changed) + @($untracked) | Where-Object { $_ } | Select-Object -Unique

        $crates = @{}
        foreach ($f in $allFiles) {
            $dir = Split-Path -Parent (Join-Path $Root $f)
            while ($dir -and (Test-Path $dir)) {
                $cargoToml = Join-Path $dir 'Cargo.toml'
                if (Test-Path $cargoToml) {
                    $nameLine = Select-String -Path $cargoToml -Pattern '^\s*name\s*=\s*"([^"]+)"' | Select-Object -First 1
                    if ($nameLine) {
                        $crateName = $nameLine.Matches[0].Groups[1].Value
                        $crates[$crateName] = $true
                    }
                    break
                }
                $parent = Split-Path -Parent $dir
                if ($parent -eq $dir -or -not $parent.StartsWith($Root)) { break }
                $dir = $parent
            }
        }
        return @($crates.Keys)
    } finally {
        Pop-Location
    }
}

Push-Location $repoRoot
try {
    # Discover every worktree linked to this repo (main + `git worktree add`
    # checkouts), not just $repoRoot. Standing convention here is to do all
    # branch work in a dedicated worktree, so uncommitted .rs changes usually
    # live there, invisible to a diff scoped only to the main tree.
    # `git worktree list` is git's own authoritative registry — deterministic,
    # no session/LLM context required to find them.
    $worktreePaths = @($repoRoot)
    $wtList = git worktree list --porcelain 2>$null
    foreach ($line in $wtList) {
        if ($line -match '^worktree\s+(.+)$') {
            # git always emits forward slashes here, but Join-Path/Split-Path
            # normalize to backslash on Windows - without this, the StartsWith
            # boundary check in Get-ChangedRustCrates silently mismatches
            # (`\`-path never StartsWith a `/`-path) and breaks out of the
            # Cargo.toml walk-up before it ever finds one.
            $wtPath = $Matches[1].Trim().Replace('/', '\')
            if ($wtPath -and ($worktreePaths -notcontains $wtPath)) {
                $worktreePaths += $wtPath
            }
        }
    }

    $failures = @()
    foreach ($root in $worktreePaths) {
        if (-not (Test-Path $root)) { continue }
        $crates = Get-ChangedRustCrates -Root $root
        if (-not $crates -or $crates.Count -eq 0) { continue }

        Push-Location $root
        try {
            foreach ($crate in $crates) {
                $out = cmd /c "cargo check -p $crate --message-format=short 2>&1"
                if ($LASTEXITCODE -ne 0) {
                    $tail = ($out | Select-Object -Last 50) -join "`n"
                    $failures += "[$root] cargo check -p $crate failed:`n$tail"
                }
            }
        } finally {
            Pop-Location
        }
    }

    if ($failures.Count -eq 0) { exit 0 }

    $combined = ($failures -join "`n`n")
    [Console]::Error.WriteLine($combined)
    exit 2
} finally {
    Pop-Location
}
