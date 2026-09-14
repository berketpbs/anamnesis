# Install anamnesis from a GitHub release.
#
#   irm https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.ps1 | iex
#
# Settings, all optional, as environment variables because a script piped into
# iex takes no parameters:
#   ANAMNESIS_VERSION      a tag such as v1.1.0; the latest release when unset
#   ANAMNESIS_INSTALL_DIR  where the binary goes (see below for the default)
#   ANAMNESIS_NO_PATH      set to anything to leave the user PATH alone
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed: a release nobody verifies is a release nobody should run.
#
# Where it goes matters more than it looks. Hooks, the MCP registration and the
# scheduled task all name the binary by its path, so an anamnesis that is
# already installed is replaced where it is, and a new one goes to
# %LOCALAPPDATA%\Programs\anamnesis rather than to Downloads.
#
# ASCII only: Windows PowerShell 5.1 reads a script without a byte order mark
# in the machine's code page, and this repository refuses byte order marks.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = 'berketpbs/anamnesis'
$target = 'x86_64-pc-windows-msvc'

function Fail([string] $message) {
    throw "anamnesis install: $message"
}

# GitHub answers a release download with a 504 now and then: on 2026-09-14 both
# CI runs of the install job failed that way on three systems while brew, which
# retries, fetched the same files in the same runs. A server error, a 408, a 429
# or a connection that got no answer is tried seven times in all, waiting 1, 2,
# 4, 8, 16 and 32 seconds; anything else - a 404 for a release that does not
# exist - fails at once. Five attempts over fifteen seconds were not enough:
# later the same day GitHub answered that way for more than half a minute.
# Windows PowerShell 5.1 has no retry of its own.
function Invoke-Retried([scriptblock] $action) {
    for ($attempt = 1; ; $attempt++) {
        try {
            return & $action
        } catch [Net.WebException] {
            $status = 0
            if ($_.Exception.Response) { $status = [int] $_.Exception.Response.StatusCode }
            $transient = ($status -eq 0) -or ($status -ge 500) -or ($status -eq 408) -or ($status -eq 429)
            if (-not $transient -or $attempt -ge 7) { throw }
            Start-Sleep -Seconds ([int] [math]::Pow(2, $attempt - 1))
        }
    }
}

if (-not [Environment]::Is64BitOperatingSystem) {
    Fail 'no release is built for 32-bit Windows'
}

# The latest tag, read from where /releases/latest redirects rather than from
# the API, which limits unauthenticated callers to sixty requests an hour.
$version = $env:ANAMNESIS_VERSION
if (-not $version) {
    try {
        $location = Invoke-Retried {
            $request = [Net.HttpWebRequest]::Create("https://github.com/$repo/releases/latest")
            $request.AllowAutoRedirect = $false
            $request.Method = 'HEAD'
            $response = $request.GetResponse()
            try { $response.Headers['Location'] } finally { $response.Close() }
        }
    } catch {
        Fail "could not reach github.com to find the latest release: $($_.Exception.Message)"
    }
    $version = ($location -split '/')[-1]
    if ($version -notlike 'v*') { Fail "could not tell the latest release from '$location'" }
}

$name = "anamnesis-$version-$target"
$archive = "$name.zip"
$base = "https://github.com/$repo/releases/download/$version"

if ($env:ANAMNESIS_INSTALL_DIR) {
    $dir = $env:ANAMNESIS_INSTALL_DIR
} else {
    $onPath = Get-Command anamnesis -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    $besideData = Join-Path $env:APPDATA 'anamnesis\bin\anamnesis.exe'
    if ($onPath) {
        $dir = Split-Path -Parent $onPath.Source
    } elseif (Test-Path $besideData) {
        $dir = Split-Path -Parent $besideData
    } else {
        $dir = Join-Path $env:LOCALAPPDATA 'Programs\anamnesis'
    }
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("anamnesis-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $work | Out-Null
try {
    Write-Host "anamnesis $version for $target"
    try {
        Invoke-Retried { Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile (Join-Path $work $archive) }
        Invoke-Retried { Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $work 'SHA256SUMS') }
    } catch {
        Fail "could not download $version from $base`: $($_.Exception.Message)"
    }

    $expected = $null
    foreach ($line in Get-Content (Join-Path $work 'SHA256SUMS')) {
        $parts = $line -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $archive) { $expected = $parts[0] }
    }
    if (-not $expected) { Fail "SHA256SUMS has no line for $archive" }
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $work $archive)).Hash
    if ($actual -ne $expected) {
        Fail "$archive does not match SHA256SUMS (expected $expected, got $actual); nothing was installed"
    }
    Write-Host '  checksum   matches SHA256SUMS'

    Expand-Archive -Path (Join-Path $work $archive) -DestinationPath $work -Force
    $new = Join-Path $work "$name\anamnesis.exe"
    if (-not (Test-Path $new)) { Fail "$archive does not hold $name\anamnesis.exe" }

    # Started once where it was unpacked, before it replaces anything: a binary
    # this machine cannot run must fail here and leave a working install as it
    # was. A native program that fails does not throw, so the exit code is the
    # only thing that says so.
    $versionLine = & $new --version
    if ($LASTEXITCODE -ne 0) {
        $code = '0x{0:X8}' -f $LASTEXITCODE
        if ($LASTEXITCODE -eq -1073741515) {
            Fail "anamnesis.exe could not start ($code, a DLL it needs is missing; usually the Microsoft Visual C++ Redistributable); nothing was installed"
        }
        Fail "anamnesis.exe --version exited with $code; nothing was installed"
    }

    New-Item -ItemType Directory -Force $dir | Out-Null
    $destination = Join-Path $dir 'anamnesis.exe'
    # Windows will not overwrite a running exe, and a server or an MCP server
    # may well be running this one. It does allow renaming it, and the running
    # process carries on from the renamed file.
    if (Test-Path $destination) {
        $aside = "$destination.old-" + (Get-Date -Format 'yyyyMMdd-HHmmss')
        Move-Item $destination $aside
        Write-Host "  kept       the previous binary as $(Split-Path -Leaf $aside)"
    }
    Copy-Item $new $destination
    Write-Host "  installed  $destination"
    Write-Host "  version    $versionLine"
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries -notcontains $dir) {
    if ($env:ANAMNESIS_NO_PATH) {
        Write-Host ''
        Write-Host "  $dir is not on PATH; ANAMNESIS_NO_PATH left it that way."
    } else {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
        Write-Host "  PATH       added $dir for this user; open a new terminal to use it"
    }
}

Write-Host ''
Write-Host '  A server that is already running keeps the old binary until it restarts.'
Write-Host '  Next, inside a repository you want remembered:'
Write-Host '    anamnesis setup'
