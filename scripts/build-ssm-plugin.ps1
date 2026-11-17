# build-ssm-plugin.ps1 - build AWS's session-manager-plugin FROM SOURCE for
# Windows and put it where Tauri's bundle.externalBin expects it:
#
#     app\src-tauri\binaries\session-manager-plugin-x86_64-pc-windows-msvc.exe
#
# The Windows runner's half of scripts/build-ssm-plugin.sh - read that header
# for the why. The same pins and the same build, so it must produce the same
# bytes (Go builds are reproducible across hosts); keep the two in step.
#
#   1. Source: AWS's tag 1.2.896.0 by COMMIT, as GitHub's archive of that
#      commit, SHA-256 checked before extraction (and its top directory must
#      name the commit).
#   2. Toolchain: Go, pinned, SHA-256 as published by go.dev, checked before use.
#   3. AWS's own build (GOPATH mode over the vendored deps, -ldflags "-s -w"),
#      version stamped to the tag, made reproducible (-trimpath, no VCS stamp,
#      empty build id, CGO off, GOAMD64=v1, nothing inherited).
#   4. The output must equal the pinned SHA-256.
#
# The result is UNSIGNED: in a release build Tauri presents it to the sign
# command (windows-sign.ps1 signs it with our certificate), and
# windows-verify-signatures.ps1 requires our signature on it in both installers.
#
# Needs tar.exe (bsdtar, in every Windows 10+). Idempotent.

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression.FileSystem

$PluginVersion = '1.2.896.0'
$PluginCommit  = '6430675a44032a551be1937fb27a2d3fdfeb27f7'
$SourceUrl     = "https://codeload.github.com/aws/session-manager-plugin/tar.gz/$PluginCommit"
$SourceSha     = '7C95BE411AD5F43D14D01460500BFD5A5AA0E28BA94815F8B049AF33A2C381B0'
$GoVersion     = '1.26.11'
$GoSha         = '71AE086F0137E665F855A811CC119152B6A783E0BBC1B906E709F46D80141EB0'   # go1.26.11.windows-amd64.zip, go.dev
$BinarySha     = '6D428E5FD77E76D9713F0C42B8EC39F2DE1031E5F3C567FEB73C7FE11B14B226'
$Triple        = 'x86_64-pc-windows-msvc'

$repoRoot = Split-Path -Parent $PSScriptRoot
$destDir  = Join-Path $repoRoot 'app\src-tauri\binaries'
$dest     = Join-Path $destDir "session-manager-plugin-$Triple.exe"
$notices  = Join-Path $repoRoot 'app\src-tauri\THIRD_PARTY_NOTICES'

function Get-Sha256([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant() }

if (-not ((Select-String -LiteralPath $notices -SimpleMatch "Version:  $PluginVersion" -Quiet) -and
          (Select-String -LiteralPath $notices -SimpleMatch "Commit:   $PluginCommit" -Quiet))) {
    throw "$notices does not name plugin $PluginVersion at $PluginCommit - update it with the bump"
}

if ((Test-Path -LiteralPath $dest) -and ((Get-Sha256 $dest) -eq $BinarySha)) {
    Write-Host "session-manager-plugin-$Triple.exe: already the pinned $PluginVersion build"
    exit 0
}

# A short work root, not %TEMP% (on the runner that is deep under
# systemprofile): the longest extracted path is ~200 characters, and MAX_PATH
# is 260 for Windows PowerShell 5.1, tar.exe and the Go build alike.
$work = Join-Path "$env:SystemDrive\" ('ssm-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
# A directory at the drive root inherits C:\'s ACL, where Authenticated Users
# may create and modify files - another local user could swap the source, the
# toolchain or the output between our checks and their use. So before anything
# lands in it: no inheritance, full control for SYSTEM, Administrators and this
# process's own user only. Mandatory on Windows; skipped only where there is no
# Windows ACL (the Linux pwsh test path).
if ($env:OS -eq 'Windows_NT') {
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    & icacls.exe $work /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*${me}:(OI)(CI)F" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
        throw "icacls could not restrict $work ($LASTEXITCODE)"
    }
}
$ProgressPreference = 'SilentlyContinue'
try {
    $goZip = Join-Path $work 'go.zip'
    Write-Host "Downloading Go $GoVersion (windows-amd64)"
    Invoke-WebRequest -Uri "https://go.dev/dl/go$GoVersion.windows-amd64.zip" -OutFile $goZip -UseBasicParsing
    $actual = Get-Sha256 $goZip
    if ($actual -ne $GoSha) { throw "SHA-256 mismatch for the Go toolchain: got $actual, pinned $GoSha" }
    [IO.Compression.ZipFile]::ExtractToDirectory($goZip, (Join-Path $work 'toolchain'))
    $go = Join-Path $work 'toolchain\go\bin\go.exe'

    $srcTgz = Join-Path $work 'source.tar.gz'
    Write-Host "Downloading session-manager-plugin $PluginVersion source ($PluginCommit)"
    Invoke-WebRequest -Uri $SourceUrl -OutFile $srcTgz -UseBasicParsing
    $actual = Get-Sha256 $srcTgz
    if ($actual -ne $SourceSha) { throw "SHA-256 mismatch for the plugin source: got $actual, pinned $SourceSha" }
    $srcDir = Join-Path $work 'src'
    New-Item -ItemType Directory -Path $srcDir | Out-Null
    # Only what the build reads, as build-ssm-plugin.sh does: the vendored SDK's
    # Java codegen/ tree reaches 258 characters inside the archive, past
    # MAX_PATH, and its snapshot/ files are test data.
    & tar.exe --exclude='*/codegen/*' --exclude='*/snapshot/*' -xzf $srcTgz -C $srcDir
    if ($LASTEXITCODE -ne 0) { throw "tar.exe could not extract the source ($LASTEXITCODE)" }
    $goSpace = Join-Path $srcDir "session-manager-plugin-$PluginCommit"
    if (-not (Test-Path -LiteralPath (Join-Path $goSpace 'src\sessionmanagerplugin-main'))) {
        throw "the source archive is not the pinned commit's tree"
    }

    # A clean, fully specified Go environment for everything below.
    foreach ($v in 'GOFLAGS', 'GOROOT', 'GOBIN', 'GOEXPERIMENT', 'GOTMPDIR') { Remove-Item "env:$v" -ErrorAction SilentlyContinue }
    $gopath = Join-Path $work 'gopath'
    $env:GOENV = 'off'; $env:GOTOOLCHAIN = 'local'; $env:GO111MODULE = 'off'; $env:GOPROXY = 'off'; $env:GOWORK = 'off'
    $env:GOPATH = "$gopath;$(Join-Path $goSpace 'vendor')"
    $env:GOCACHE = Join-Path $work 'cache'; $env:GOMODCACHE = Join-Path $work 'modcache'
    $env:CGO_ENABLED = '0'; $env:GOAMD64 = 'v1'; $env:GOARM64 = 'v8.0'

    # makefile pre-build: stamp the version with AWS's own generator.
    [IO.File]::WriteAllText((Join-Path $goSpace 'VERSION'), $PluginVersion)
    # version-gen runs on this host, so no GOOS/GOARCH yet.
    Remove-Item env:GOOS, env:GOARCH -ErrorAction SilentlyContinue
    Push-Location $goSpace
    try { & $go run ./src/version/versiongenerator/version-gen.go | Out-Null } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "version-gen failed ($LASTEXITCODE)" }
    if (-not (Select-String -LiteralPath (Join-Path $goSpace 'src\version\version.go') -SimpleMatch "const Version = `"$PluginVersion`"" -Quiet)) {
        throw 'the version stamp did not take'
    }

    # makefile copy-src: GOPATH mode, from a copy at the import path.
    $pkgRoot = Join-Path $gopath 'src\github.com\aws\session-manager-plugin'
    New-Item -ItemType Directory -Force -Path $pkgRoot | Out-Null
    Copy-Item -Recurse -LiteralPath (Join-Path $goSpace 'src') -Destination $pkgRoot

    $out = Join-Path $work 'session-manager-plugin.exe'
    Write-Host "Building session-manager-plugin $PluginVersion for windows/amd64"
    $env:GOOS = 'windows'; $env:GOARCH = 'amd64'
    Push-Location $pkgRoot
    try {
        & $go build -trimpath -buildvcs=false -ldflags '-s -w -buildid=' -o $out `
            github.com/aws/session-manager-plugin/src/sessionmanagerplugin-main
    } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "go build failed ($LASTEXITCODE)" }
    $actual = Get-Sha256 $out
    if ($actual -ne $BinarySha) {
        throw "the windows/amd64 build is not the pinned binary: got $actual, pinned $BinarySha (source, toolchain or build flags changed)"
    }

    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Move-Item -LiteralPath $out -Destination $dest -Force
    # And what landed is still what was checked.
    $actual = Get-Sha256 $dest
    if ($actual -ne $BinarySha) {
        Remove-Item -LiteralPath $dest -Force -ErrorAction SilentlyContinue
        throw "$dest changed after it was verified: got $actual, pinned $BinarySha"
    }
    Write-Host "session-manager-plugin-$Triple.exe: session-manager-plugin $PluginVersion (unsigned; signed by the release build)"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    # PS 5.1's Remove-Item can give up on read-only files and deep trees;
    # rd /s /q is the fallback.
    if (Test-Path -LiteralPath $work) { cmd /c rd /s /q "`"$work`"" | Out-Null }
    if (Test-Path -LiteralPath $work) { Write-Warning "could not remove $work" }
}
