# run-scenario.ps1 -- Windows-side per-scenario executor (generic).
#
# Invoked by run-matrix.rs (one process per scenario). Reads a fully
# resolved scenario JSON written by the runner -- this JSON carries:
#
#   {
#     "name": "...",
#     "image": "abs path on VM",
#     "rw": true|false,
#     "mount": {
#         "command":     "<binary> mount {image} --drive {drive} {extra}",
#         "ready_line":  "ext4 mounted at",     // regex; "" => no wait
#         "extra":       "--rw"                  // optional
#     },
#     "ops":      [ { "type": "ls", ... }, ... ],
#     "templates": { "ls": "...", "cat": "...", ... },
#     "post_verify": null | {"command": "fsck.ext4 -fn {image}", "expect_exit": 0}
#   }
#
# Stages:
#   A. Resolve image, pick drive letter.
#   B. If [mount] declared: spawn mount command, wait for ready_line.
#   C. Iterate ops[]. Each op resolves to a template substitution OR a
#      built-in PS file op (write/mkdir/unlink/rmdir/rename via the
#      mounted drive). Expectations checked generically.
#   D. Stop mount process (Stop-Process -Force; mounts are expected to
#      be tolerant of forced termination via flush-on-exit).
#   E. Run post_verify command if declared.
#   F. Write manifest.json + op-trace.jsonl. Emit VERDICT= marker.

param(
    [Parameter(Mandatory=$true)] [string]$ScenarioName,
    [Parameter(Mandatory=$true)] [string]$ScenarioJson,
    [Parameter(Mandatory=$true)] [string]$Diag
)

$ErrorActionPreference = 'Continue'

function Write-OpTrace {
    param([string]$DiagDir, [hashtable]$Entry)
    $line = ($Entry | ConvertTo-Json -Compress -Depth 8)
    Add-Content -LiteralPath (Join-Path $DiagDir 'op-trace.jsonl') -Value $line
}

function Get-Sha256OfBytes {
    param([byte[]]$Bytes)
    if ($null -eq $Bytes -or $Bytes.Length -eq 0) {
        return 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha.ComputeHash($Bytes)
        return ([System.BitConverter]::ToString($hash) -replace '-','').ToLower()
    } finally { $sha.Dispose() }
}

function Find-FreeDriveLetter {
    $used = @()
    Get-PSDrive -PSProvider FileSystem -EA SilentlyContinue |
        ForEach-Object { $used += $_.Name.ToUpper() }
    Get-Volume -EA SilentlyContinue |
        Where-Object { $_.DriveLetter } |
        ForEach-Object { $used += "$($_.DriveLetter)".ToUpper() }
    foreach ($code in 68..90) {                # D..Z
        $c = [char]$code
        if (("$c" ).ToUpper() -notin $used) { return "$c`:" }
    }
    throw "no free drive letter found in D..Z"
}

# Substitute {token} placeholders in a template string against a hashtable.
# Plain (non-regex) string replacement so Windows paths -- which contain
# `\`, a regex metachar AND a -replace backreference char -- pass
# through unmolested.
function Expand-Template {
    param([string]$Template, [hashtable]$Vars)
    $out = $Template
    foreach ($k in $Vars.Keys) {
        $out = $out.Replace("{$k}", "$($Vars[$k])")
    }
    # Replace remaining empty-substitution tokens with empty string.
    $out = $out -replace '\{[a-zA-Z_][a-zA-Z0-9_]*\}', ''
    return $out
}

# Run "<exe> arg1 arg2 ..." as a single shell command via cmd /c, capture
# stdout / stderr / exit code.
function Invoke-CommandLine {
    param([string]$CommandLine, [string]$DiagDir, [int]$OpIndex)
    $stdoutPath = Join-Path $DiagDir ("op{0:D2}-stdout.txt" -f $OpIndex)
    $stderrPath = Join-Path $DiagDir ("op{0:D2}-stderr.txt" -f $OpIndex)
    $proc = Start-Process -FilePath 'cmd.exe' `
        -ArgumentList @('/c', $CommandLine) `
        -NoNewWindow -PassThru -Wait `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError  $stderrPath
    return @{
        exit   = $proc.ExitCode
        stdout = (Get-Content -Raw -LiteralPath $stdoutPath -EA SilentlyContinue)
        stderr = (Get-Content -Raw -LiteralPath $stderrPath -EA SilentlyContinue)
    }
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
Remove-Item -LiteralPath (Join-Path $Diag 'op-trace.jsonl') -EA SilentlyContinue

# Best-effort cleanup of stale mount processes from a previously-crashed
# scenario. We can't reliably name-filter to "this consumer's binary",
# but the consumer name + cmd.exe child pattern catches the common case
# where a Start-Process cmd.exe wrapper is still alive holding stdout.
$staleProcessName = $null
if ($scenario.mount -and $scenario.mount.command) {
    $cmdHead = ($scenario.mount.command -split '\s+')[0]
    $staleProcessName = [System.IO.Path]::GetFileNameWithoutExtension($cmdHead)
}
if ($staleProcessName) {
    Get-Process -Name $staleProcessName -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    # Tiny grace so handles release before the next Start-Process opens
    # the same RedirectStandardOutput path.
    Start-Sleep -Milliseconds 200
}

$scenario = Get-Content -Raw -LiteralPath $ScenarioJson | ConvertFrom-Json

$manifest = [ordered]@{
    scenario_name = $ScenarioName
    image         = "$($scenario.image)"
    rw            = [bool]$scenario.rw
    started_at    = (Get-Date).ToUniversalTime().ToString('o')
    ended_at      = $null
    drive_letter  = $null
    mount_pid     = $null
    operations    = @()
    verdict       = 'errored'
    error         = $null
}

$mountProc = $null

try {
    # ---- Stage A: resolve image -------------------------------------
    if ("$($scenario.image)" -and -not (Test-Path -LiteralPath $scenario.image)) {
        throw "image not found: $($scenario.image)"
    }

    # Pick drive letter (used both as substitution token + for builtin ops).
    $drive = if ($scenario.drive) { "$($scenario.drive)" } else { Find-FreeDriveLetter }
    $manifest.drive_letter = $drive
    $driveBare = $drive.TrimEnd(':')

    # ---- Stage B: mount (if declared) -------------------------------
    if ($scenario.mount -and $scenario.mount.command) {
        $vars = @{
            image = $scenario.image
            drive = $drive
            extra = if ($scenario.mount.extra) { $scenario.mount.extra } else { '' }
        }
        $mountCmd = Expand-Template -Template $scenario.mount.command -Vars $vars
        $mountStdout = Join-Path $Diag 'mount-stdout.txt'
        $mountStderr = Join-Path $Diag 'mount-stderr.txt'
        # Remove any stale files from a prior run before Start-Process
        # opens them. Pre-creating with Set-Content collides with the
        # child process's RedirectStandardOutput on Windows; deletion
        # is the safer reset.
        Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $mountStdout
        Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $mountStderr
        $mountProc = Start-Process -FilePath 'cmd.exe' `
            -ArgumentList @('/c', $mountCmd) `
            -NoNewWindow -PassThru `
            -RedirectStandardOutput $mountStdout `
            -RedirectStandardError  $mountStderr
        $manifest.mount_pid = $mountProc.Id

        $readyLine = "$($scenario.mount.ready_line)"
        if ($readyLine) {
            $mounted = $false
            for ($i = 0; $i -lt 60; $i++) {
                Start-Sleep -Milliseconds 500
                if ($mountProc.HasExited) {
                    $stderr = Get-Content -Raw -LiteralPath $mountStderr -EA SilentlyContinue
                    throw "mount exited prematurely (code=$($mountProc.ExitCode)): $stderr"
                }
                $out = Get-Content -Raw -LiteralPath $mountStdout -EA SilentlyContinue
                if ($out -and ($out -match $readyLine)) { $mounted = $true; break }
            }
            if (-not $mounted) {
                throw "mount did not signal ready within 30s; see $mountStdout / $mountStderr"
            }
            Start-Sleep -Milliseconds 500   # let the FS layer settle
        }
    }

    # ---- Stage C: ops ----------------------------------------------
    $opIndex = 0
    $allOk = $true
    $templates = @{}
    if ($scenario.templates) {
        foreach ($p in $scenario.templates.PSObject.Properties) {
            $templates[$p.Name] = "$($p.Value)"
        }
    }
    foreach ($op in @($scenario.ops)) {
        $opIndex++
        $opStart = Get-Date
        $opType = "$($op.type)"
        $rec = [ordered]@{
            index    = $opIndex
            type     = $opType
            input    = $op
            ok       = $false
            error    = $null
            output   = $null
            duration_ms = 0
        }
        try {
            # Resolve substitution variables for the op.
            $vars = @{
                image = $scenario.image
                drive = $drive
                path  = if ($null -ne $op.path) { "$($op.path)" } else { '' }
                from  = if ($null -ne $op.from) { "$($op.from)" } else { '' }
                to    = if ($null -ne $op.to)   { "$($op.to)"   } else { '' }
                extra = if ($null -ne $op.extra){ "$($op.extra)"} else { '' }
                content = if ($null -ne $op.content){ "$($op.content)"} else { '' }
            }

            $template = $templates[$opType]
            $output = $null
            if ($template) {
                # Template-driven path: run the command, capture stdout, apply
                # generic expectations.
                $expanded = Expand-Template -Template $template -Vars $vars
                $r = Invoke-CommandLine -CommandLine $expanded -DiagDir $Diag -OpIndex $opIndex
                if ($r.exit -ne 0 -and -not $op.allow_nonzero_exit) {
                    throw "op '$opType' exit $($r.exit): $($r.stderr.Trim())"
                }
                $stdoutBytes = [System.Text.Encoding]::UTF8.GetBytes("$($r.stdout)")
                $sha = Get-Sha256OfBytes -Bytes $stdoutBytes
                $output = @{
                    exit       = $r.exit
                    stdout_sha256 = $sha
                    stdout_len = $stdoutBytes.Length
                }
                # expect_names: parse stdout into whitespace-separated lines, last column
                if ($null -ne $op.expect_names) {
                    $names = @()
                    foreach ($line in (("$($r.stdout)") -split "(`r`n|`r|`n)")) {
                        $s = "$line".Trim()
                        if (-not $s) { continue }
                        $cols = $s -split '\s+'
                        $names += $cols[$cols.Length - 1]
                    }
                    $names = $names | Sort-Object
                    $expected = @($op.expect_names) | Sort-Object
                    $output['names'] = $names
                    $diff = Compare-Object -ReferenceObject $expected -DifferenceObject $names
                    if ($diff) {
                        throw "names mismatch. expected=$($expected -join ','); got=$($names -join ',')"
                    }
                }
                if ($null -ne $op.expect_count) {
                    $cnt = ((("$($r.stdout)") -split "(`r`n|`r|`n)") |
                        Where-Object { "$_".Trim() }).Count
                    $output['count'] = $cnt
                    if ($cnt -ne [int]$op.expect_count) {
                        throw "count mismatch. expected=$($op.expect_count); got=$cnt"
                    }
                }
                if ($null -ne $op.expect_stdout_sha256) {
                    if ($sha -ne ("$($op.expect_stdout_sha256)").ToLower()) {
                        throw "stdout sha256 mismatch. expected=$($op.expect_stdout_sha256); got=$sha"
                    }
                }
                if ($null -ne $op.expect_stdout_contains) {
                    if ("$($r.stdout)" -notmatch [regex]::Escape("$($op.expect_stdout_contains)")) {
                        throw "stdout missing expected substring: $($op.expect_stdout_contains)"
                    }
                }
                if ($null -ne $op.expect_exit) {
                    if ($r.exit -ne [int]$op.expect_exit) {
                        throw "exit mismatch. expected=$($op.expect_exit); got=$($r.exit)"
                    }
                }
            } else {
                # Built-in fallback for ops with no template; uses the
                # mounted drive directly.
                $mntPath = if ($vars.path) {
                    "${driveBare}:\$($vars.path -replace '^/','' -replace '/','\')"
                } else { $null }
                switch ($opType) {
                    'write' {
                        if (-not $manifest.rw) { throw "'write' op requires rw mount" }
                        if ($null -ne $op.content_b64) {
                            $bytes = [System.Convert]::FromBase64String("$($op.content_b64)")
                        } elseif ($null -ne $op.content) {
                            $bytes = [System.Text.Encoding]::UTF8.GetBytes("$($op.content)")
                        } else {
                            throw "'write' requires content or content_b64"
                        }
                        [System.IO.File]::WriteAllBytes($mntPath, $bytes)
                        $output = @{ wrote_bytes = $bytes.Length; path = $mntPath }
                    }
                    'mkdir' {
                        if (-not $manifest.rw) { throw "'mkdir' op requires rw mount" }
                        New-Item -ItemType Directory -Path $mntPath -Force | Out-Null
                        $output = @{ created = $mntPath }
                    }
                    'unlink' {
                        if (-not $manifest.rw) { throw "'unlink' op requires rw mount" }
                        Remove-Item -LiteralPath $mntPath -Force
                        $output = @{ removed = $mntPath }
                    }
                    'rmdir' {
                        if (-not $manifest.rw) { throw "'rmdir' op requires rw mount" }
                        Remove-Item -LiteralPath $mntPath -Force -Recurse:$false
                        $output = @{ removed_dir = $mntPath }
                    }
                    'rename' {
                        if (-not $manifest.rw) { throw "'rename' op requires rw mount" }
                        $src = "${driveBare}:\$($op.from -replace '^/','' -replace '/','\')"
                        $dst = "${driveBare}:\$($op.to   -replace '^/','' -replace '/','\')"
                        Rename-Item -LiteralPath $src -NewName $dst -Force
                        $output = @{ renamed = "$src -> $dst" }
                    }
                    'cat_via_mount' {
                        $bytes = [System.IO.File]::ReadAllBytes($mntPath)
                        $sha = Get-Sha256OfBytes -Bytes $bytes
                        $output = @{ sha256 = $sha; len = $bytes.Length }
                        if ($null -ne $op.expect_sha256) {
                            if ($sha -ne ("$($op.expect_sha256)").ToLower()) {
                                throw "cat_via_mount sha256 mismatch. expected=$($op.expect_sha256); got=$sha"
                            }
                        }
                        if ($null -ne $op.expect_content) {
                            $expBytes = [System.Text.Encoding]::UTF8.GetBytes("$($op.expect_content)")
                            $expSha = Get-Sha256OfBytes -Bytes $expBytes
                            if ($sha -ne $expSha) {
                                throw "cat_via_mount content mismatch. expected=$($op.expect_content); got_sha=$sha"
                            }
                        }
                        if ($null -ne $op.expect_size) {
                            if ($bytes.Length -ne [int]$op.expect_size) {
                                throw "cat_via_mount size mismatch. expected=$($op.expect_size); got=$($bytes.Length)"
                            }
                        }
                    }
                    default {
                        throw "no template for op '$opType' and no built-in fallback"
                    }
                }
            }
            $rec.output = $output
            $rec.ok = $true
        } catch {
            $rec.ok = $false
            $rec.error = "$_"
            $allOk = $false
        }
        $rec.duration_ms = [int]((Get-Date) - $opStart).TotalMilliseconds
        Write-OpTrace -DiagDir $Diag -Entry $rec
        $manifest.operations += @($rec)
        if (-not $rec.ok) { break }
    }

    if ($allOk) { $manifest.verdict = 'passed' } else { $manifest.verdict = 'failed' }
} catch {
    $manifest.error = "$_"
    $manifest.verdict = 'errored'
} finally {
    # ---- Stage D: stop mount ---------------------------------------
    # `Stop-Process` kills only the named process. Mount commands are
    # spawned through `cmd.exe /c <binary> mount ...` so the actual
    # mount lives one level deeper as a child of cmd.exe. If we kill
    # only cmd.exe the binary inherits the orphaned stdout/stderr and
    # cargo / ssh / the runner all wait on it forever.
    #
    # `taskkill /T /F` (terminate-tree, force) walks the descendant
    # tree and kills every member — the right primitive for tearing
    # down the wrapper + mount + any winfsp helpers in one shot.
    try {
        if ($manifest.mount_pid) {
            $p = Get-Process -Id $manifest.mount_pid -EA SilentlyContinue
            if ($p) {
                & taskkill.exe /T /F /PID $manifest.mount_pid 2>&1 | Out-Null
                for ($i = 0; $i -lt 20; $i++) {
                    Start-Sleep -Milliseconds 500
                    if (-not (Get-Process -Id $manifest.mount_pid -EA SilentlyContinue)) { break }
                }
            }
        }
        # Defence in depth: anything matching the consumer's binary name
        # that's still alive is a leaked WinFsp host from this or a
        # previous scenario. Kill it before the next scenario reuses
        # the drive letter.
        if ($staleProcessName) {
            Get-Process -Name $staleProcessName -EA SilentlyContinue |
                ForEach-Object { & taskkill.exe /T /F /PID $_.Id 2>&1 | Out-Null }
        }
    } catch { }

    # ---- Stage E: post-verify --------------------------------------
    if ($manifest.verdict -eq 'passed' -and $scenario.post_verify -and $scenario.post_verify.command) {
        $pvVars = @{
            image = $scenario.image
            drive = $drive
        }
        $pvCmd = Expand-Template -Template $scenario.post_verify.command -Vars $pvVars
        $pvStdout = Join-Path $Diag 'post-verify-stdout.txt'
        $pvStderr = Join-Path $Diag 'post-verify-stderr.txt'
        $pvProc = Start-Process -FilePath 'cmd.exe' `
            -ArgumentList @('/c', $pvCmd) `
            -NoNewWindow -PassThru -Wait `
            -RedirectStandardOutput $pvStdout `
            -RedirectStandardError  $pvStderr
        $expectExit = if ($null -ne $scenario.post_verify.expect_exit) {
            [int]$scenario.post_verify.expect_exit
        } else { 0 }
        if ($pvProc.ExitCode -ne $expectExit) {
            $manifest.verdict = 'failed'
            $manifest.error = "post-verify exit $($pvProc.ExitCode) != expected $expectExit"
        }
        Write-Output "POST_VERIFY_EXIT=$($pvProc.ExitCode)"
    }

    # ---- Stage F: manifest ----------------------------------------
    $manifest.ended_at = (Get-Date).ToUniversalTime().ToString('o')
    $manifestJson = ($manifest | ConvertTo-Json -Depth 10)
    Set-Content -LiteralPath (Join-Path $Diag 'manifest.json') -Value $manifestJson -Encoding UTF8

    Write-Output "VERDICT=$($manifest.verdict)"
    if ($manifest.error) {
        $err1 = ($manifest.error -replace "[\r\n]+", ' / ')
        Write-Output "ERROR=$err1"
    }
}
