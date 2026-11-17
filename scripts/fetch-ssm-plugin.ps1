# fetch-ssm-plugin.ps1 - put AWS's session-manager-plugin where Tauri's
# bundle.externalBin expects it on Windows:
#
#     app\src-tauri\binaries\session-manager-plugin-x86_64-pc-windows-msvc.exe
#
# The Windows half of scripts/fetch-ssm-plugin.sh (read its header for the
# why). Same trust chain, and the build fails on any miss:
#   1. A PINNED version, downloaded only from AWS's own S3 path
#      (windows/SessionManagerPlugin.zip - the portable package AWS's own
#      install.bat unpacks; the .exe installer next to it is a bootstrapper).
#   2. A PINNED SHA-256 of that zip, checked BEFORE anything is extracted, then
#      of the package.zip inside it, then of the binary itself.
#   3. AWS's Authenticode signature on the binary: valid, chained to a trusted
#      root, timestamped, and issued to Amazon Web Services, Inc. AWS publishes
#      no GPG .sig for the Windows zip (403).
#
# We deliberately do NOT re-sign the plugin with our certificate. It already
# carries AWS's timestamped signature, which is what Windows checks, and
# Tauri itself skips a sidecar that `signtool verify /pa` accepts - so our sign
# command never sees it (windows-sign.ps1 refuses it if it ever does, rather
# than ship it unsigned or under our name). windows-verify-signatures.ps1
# checks the copy inside the MSI is still AWS-signed.
#
# Idempotent: a destination that already holds the pinned binary is left alone.

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression.FileSystem

$Version       = '1.2.896.0'
$BaseUrl       = "https://s3.amazonaws.com/session-manager-downloads/plugin/$Version"
$Artifact      = 'windows/SessionManagerPlugin.zip'
$ArtifactSha   = 'B09ADC18721B6E7A9888C98CB1A3AA55B681B8F12391FC1A983410CB1163F670'
$PackageSha    = '54577F90838E297801F79A769B4DCB47708FF0595CEBA5BF853276BC3EA83923'
$BinarySha     = '03E9F869CDCB41C02060A4E8F8A07FAABAAE05F9940EC221D4A010B481A9AE30'
# .NET quotes a DN value holding a comma (O="Amazon Web Services, Inc."), so
# accept it with or without the quotes.
$SignerOrg     = '(^|,\s*)O="?Amazon Web Services, Inc\."?(,|$)'
$Triple        = 'x86_64-pc-windows-msvc'

$repoRoot = Split-Path -Parent $PSScriptRoot
$destDir  = Join-Path $repoRoot 'app\src-tauri\binaries'
$dest     = Join-Path $destDir "session-manager-plugin-$Triple.exe"
$notices  = Join-Path $repoRoot 'app\src-tauri\THIRD_PARTY_NOTICES'

function Get-Sha256([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant() }

function Assert-AwsSigned([string]$Path) {
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    if ($sig.Status -ne 'Valid') { throw "session-manager-plugin.exe: Authenticode status $($sig.Status), expected Valid" }
    if (-not $sig.TimeStamperCertificate) { throw 'session-manager-plugin.exe: AWS signature carries no timestamp' }
    $subject = $sig.SignerCertificate.Subject
    if ($subject -notmatch $SignerOrg) { throw "session-manager-plugin.exe: signed by '$subject', not Amazon Web Services, Inc." }
    Write-Host "Signed by $subject (timestamped)"
}

if (-not (Select-String -LiteralPath $notices -SimpleMatch "Version:  $Version" -Quiet)) {
    throw "$notices does not name plugin $Version - update it with the version bump"
}

if ((Test-Path -LiteralPath $dest) -and ((Get-Sha256 $dest) -eq $BinarySha)) {
    Assert-AwsSigned $dest
    Write-Host "session-manager-plugin-$Triple.exe: already the pinned $Version binary"
    exit 0
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('ssm-plugin-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work 'SessionManagerPlugin.zip'
    Write-Host "Downloading $BaseUrl/$Artifact"
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri "$BaseUrl/$Artifact" -OutFile $zip -UseBasicParsing
    $actual = Get-Sha256 $zip
    if ($actual -ne $ArtifactSha) { throw "SHA-256 mismatch for ${Artifact}: got $actual, pinned $ArtifactSha" }

    # Extract one named entry, never the whole archive.
    function Expand-Entry([string]$Archive, [string]$Entry, [string]$Out) {
        $z = [IO.Compression.ZipFile]::OpenRead($Archive)
        try {
            $e = $z.GetEntry($Entry)
            if (-not $e) { throw "$Entry is not where AWS's package used to put it" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($e, $Out, $true)
        } finally { $z.Dispose() }
    }

    $package = Join-Path $work 'package.zip'
    Expand-Entry $zip 'package.zip' $package
    $actual = Get-Sha256 $package
    if ($actual -ne $PackageSha) { throw "SHA-256 mismatch for package.zip: got $actual, pinned $PackageSha" }

    $exe = Join-Path $work 'session-manager-plugin.exe'
    Expand-Entry $package 'bin/session-manager-plugin.exe' $exe
    $actual = Get-Sha256 $exe
    if ($actual -ne $BinarySha) { throw "SHA-256 mismatch for session-manager-plugin.exe: got $actual, pinned $BinarySha" }
    Assert-AwsSigned $exe

    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Move-Item -LiteralPath $exe -Destination $dest -Force
    Write-Host "session-manager-plugin-$Triple.exe: session-manager-plugin $Version"
} finally {
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}
