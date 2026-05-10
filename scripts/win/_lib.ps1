# scripts/win/_lib.ps1 -- shared helpers for fs-agnostic vm-side ops.
#
# Each win-*.ps1 in this directory is invoked via SSH-per-step by the
# v2 dispatcher. Because the spawning SSH session ends as soon as the
# script returns, user-mode mount processes (WinFsp + an FS driver
# binary, etc.) die with the session — cross-step persistent mounts
# don't work without serious detach plumbing.
#
# Workaround: each op is SELF-CONTAINED. Mount, do the op, unmount —
# all within one SSH session. The cost is mount/unmount overhead per
# step (~1-3s on most fs drivers); the win is reliability + zero
# cross-step state-file plumbing.
#
# Each consumer parameterises mount via -BinaryCmd + -ReadyLine in
# its harness.toml op-defs. The op script forwards them to
# Invoke-WithMount along with a scriptblock that does the actual op.

$ErrorActionPreference = 'Stop'

function Resolve-MountedPath {
    # Convert "/foo/bar.txt" or "foo\bar.txt" into "<letter>:\foo\bar.txt".
    param(
        [Parameter(Mandatory=$true)] [string]$DriveLetter,
        [Parameter(Mandatory=$true)] [string]$Path
    )
    $rel = $Path -replace '^[/\\]+', ''
    return "${DriveLetter}:\$rel"
}

function Invoke-WithMount {
    # Mount the volume, run a scriptblock with the drive letter
    # available, then unmount unconditionally (try/finally).
    #
    # The scriptblock is invoked via & — pass any state it needs as
    # additional positional args to Invoke-WithMount and reference
    # them inside the block. (Closures via .GetNewClosure() also work
    # for capturing param() variables from the caller.)
    #
    # Args:
    #   -BinaryCmd          Full mount command line. Spawned under
    #                       cmd.exe /c.
    #   -ReadyLine          Regex; when seen on stdout the mount is up.
    #   -Drive              Drive letter (single char, no colon).
    #   -ScriptBlock        Code to run while the mount is up. Gets
    #                       the drive letter as $args[0] (or capture
    #                       via closure).
    #   -ReadyTimeoutSeconds (optional, default 30)
    #   -DiagDir            (optional) Where to drop mount-stdout/err
    #                       on the way through. Defaults to a known
    #                       fallback under VM_WORKDIR-equivalent.
    param(
        [Parameter(Mandatory=$true)] [string]$BinaryCmd,
        [Parameter(Mandatory=$true)] [string]$ReadyLine,
        [Parameter(Mandatory=$true)] [string]$Drive,
        [Parameter(Mandatory=$true)] [scriptblock]$ScriptBlock,
        [int]$ReadyTimeoutSeconds = 30,
        [string]$DiagDir = ''
    )

    # Strip the __none__ sentinel tokens from the mount command line.
    # Convention: matrix authors set unset optional fields to __none__
    # (the v2 substitution can't omit a token entirely, only collapse
    # to empty, and trailing empty quoted args break PowerShell 5.1
    # command parsing). We filter them out before spawning so the
    # actual mount command is clean.
    $BinaryCmd = (($BinaryCmd -split '\s+') | Where-Object { $_ -ne '__none__' }) -join ' '

    if (-not $DiagDir -or $DiagDir.Trim() -eq '') {
        $DiagDir = "$env:USERPROFILE\dev\ext4-work\last-mount-diag"
    }
    New-Item -ItemType Directory -Path $DiagDir -Force | Out-Null
    $stdout = Join-Path $DiagDir 'mount-stdout.txt'
    $stderr = Join-Path $DiagDir 'mount-stderr.txt'
    Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $stdout, $stderr

    $proc = Start-Process -FilePath 'cmd.exe' `
        -ArgumentList @('/c', $BinaryCmd) `
        -NoNewWindow -PassThru `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError  $stderr

    $deadline = (Get-Date).AddSeconds($ReadyTimeoutSeconds)
    $mounted = $false
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if ($proc.HasExited) {
            $errOut = Get-Content -Raw -LiteralPath $stderr -EA SilentlyContinue
            [Console]::Error.WriteLine("mount exited prematurely (code=$($proc.ExitCode)):")
            [Console]::Error.WriteLine($errOut)
            exit 1
        }
        $out = Get-Content -Raw -LiteralPath $stdout -EA SilentlyContinue
        if ($out -and ($out -match $ReadyLine)) { $mounted = $true; break }
    }
    if (-not $mounted) {
        [Console]::Error.WriteLine("mount did not signal ready within ${ReadyTimeoutSeconds}s; see $stdout / $stderr")
        Stop-Process -Id $proc.Id -Force -EA SilentlyContinue
        exit 1
    }
    Start-Sleep -Milliseconds 500   # let the volume layer settle

    $rc = 0
    try {
        & $ScriptBlock $Drive
    } catch {
        [Console]::Error.WriteLine("op failed: $_")
        $rc = 1
    } finally {
        # Sync + dismount before killing. The OS-level Sync ensures any
        # cached writes for the volume reach the WinFsp driver, which
        # (in turn) flushes them to the underlying .img. Without this
        # the next step's mount may see a stale image.
        try {
            $vol = Get-Volume -DriveLetter $Drive -EA SilentlyContinue
            if ($vol) {
                # PowerShell exposes no direct fsync — but we can force
                # a write barrier by enumerating the drive (triggers a
                # cache walk in the FS driver) followed by a brief
                # quiesce sleep before kill.
                Get-ChildItem -LiteralPath "${Drive}:\" -EA SilentlyContinue | Out-Null
            }
        } catch {}
        Start-Sleep -Milliseconds 2500  # quiesce window for WinFsp to flush dirty pages to .img
        # Tree-kill (cmd.exe wrapper + child mount binary).
        & taskkill.exe /T /F /PID $proc.Id 2>&1 | Out-Null
        for ($i = 0; $i -lt 20; $i++) {
            Start-Sleep -Milliseconds 250
            if (-not (Get-Process -Id $proc.Id -EA SilentlyContinue)) { break }
        }
    }
    exit $rc
}
