# windows-signing-setup.ps1 - prepare Azure Artifact Signing for a Windows CI job.
#
# Run once per job, before `cargo tauri build`. It fetches the two things the
# build VM does not carry, checks each against a pinned hash, and hands the
# rest of the job what `windows-sign.ps1` needs through $GITHUB_ENV, including
# TRIPLE_C_TAURI_SIGN_CONFIG - a config file for `cargo tauri build --config`.
# Not the TAURI_CONFIG variable: the v2 CLI never reads it (it only *sets* it,
# for tauri-build), so a sign command put there is silently ignored.
#
#   * Microsoft.ArtifactSigning.Client - the signtool "dlib" that forwards the
#     digest to Azure instead of signing with a local certificate.
#   * the .NET runtime that dlib is hosted on. It asks for 8.0 with
#     rollForward=Major, so 10 LTS satisfies it; 8 goes out of support in
#     November 2026 and 10 is supported to 2028.
#
# Everything lands inside the job's workspace and is gone with it. Nothing is
# installed on the VM: the two runners on it (winvm-builder, virtual-builder)
# share one machine, and a system-wide install would be state neither job
# owns. The workspace, not %TEMP%, because the runners run as SYSTEM and the
# NSIS uninstaller is signed from inside 32-bit makensis: WOW64 redirects a
# 32-bit process's view of System32 - where SYSTEM's %TEMP% lives - to
# SysWOW64. The workspace sits under systemprofile\.cache, which the VM
# junctions so both views resolve (see "Build Tauri app" in build-app.yml).
#
# Bumping a pin: take the new version's hash from the publisher, never from a
# download you just made - the client's SHA-512 is the base64 `packageHash` in
# its nuget.org catalog entry (hex here), the runtime's is in .NET's
# releases.json.
#
# Required environment (repository secrets): AZURE_TENANT_ID, AZURE_CLIENT_ID,
# AZURE_CLIENT_SECRET, ARTIFACT_SIGNING_ENDPOINT, ARTIFACT_SIGNING_ACCOUNT_NAME,
# ARTIFACT_SIGNING_PROFILE_NAME. A missing one fails the job: an unsigned
# installer must not reach a release by accident.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$ClientVersion = '1.0.128'
$ClientSha512  = '98f06a691f4fc2fa22f19dcf8556733e98607fbef91a312c453b9b0798cc9088dae0acb36e389b552a11b4d2320324785b8541c2b51091a724c05bc5df5cbf95'
$ClientUrl     = "https://api.nuget.org/v3-flatcontainer/microsoft.artifactsigning.client/$ClientVersion/microsoft.artifactsigning.client.$ClientVersion.nupkg"

$DotnetVersion = '10.0.12'
$DotnetSha512  = '844fa99e16fd6f44e0a7c29def7a82d7846902334d6a955248a9519a4dddb3f5acceb9c9223bef69f8c83b8ae2417537e5b76dddf79fb7117dc85b5039bc1297'
$DotnetUrl     = "https://builds.dotnet.microsoft.com/dotnet/Runtime/$DotnetVersion/dotnet-runtime-$DotnetVersion-win-x64.zip"

$TimestampUrl  = 'http://timestamp.acs.microsoft.com'

$required = 'AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET',
            'ARTIFACT_SIGNING_ENDPOINT', 'ARTIFACT_SIGNING_ACCOUNT_NAME', 'ARTIFACT_SIGNING_PROFILE_NAME'
$missing = @($required | Where-Object { -not [Environment]::GetEnvironmentVariable($_) })
if ($missing.Count -gt 0) {
    throw "Code signing is not configured: missing $($missing -join ', '). Add them as repository secrets."
}
if (-not $env:GITHUB_ENV) { throw 'GITHUB_ENV is not set - this script only runs inside a CI job.' }

$workspace = if ($env:GITHUB_WORKSPACE) { $env:GITHUB_WORKSPACE } else { (Get-Location).Path }
$root = Join-Path $workspace '.code-signing'
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
New-Item -ItemType Directory -Path $root | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Get-Verified([string]$Url, [string]$Name, [string]$Algorithm, [string]$Expected) {
    $file = Join-Path $root $Name
    Write-Host "Downloading $Url"
    Invoke-WebRequest -Uri $Url -OutFile $file -UseBasicParsing
    $actual = (Get-FileHash -Path $file -Algorithm $Algorithm).Hash
    if ($actual -ne $Expected) {
        throw "$Name failed its $Algorithm check: expected $Expected, got $actual"
    }
    Write-Host "$Name $Algorithm verified"
    return $file
}

# The signing client. A .nupkg is a zip; extract it with the framework rather
# than Expand-Archive, which on PowerShell 5.1 refuses any extension but .zip.
$nupkg = Get-Verified $ClientUrl 'client.nupkg' 'SHA512' $ClientSha512
$clientDir = Join-Path $root 'client'
[IO.Compression.ZipFile]::ExtractToDirectory($nupkg, $clientDir)
$dlib = Join-Path $clientDir 'bin\x64\Azure.CodeSigning.Dlib.dll'
if (-not (Test-Path $dlib)) { throw "Signing client $ClientVersion has no $dlib" }

# The runtime the dlib is hosted on, found through DOTNET_ROOT.
$dotnetZip = Get-Verified $DotnetUrl 'dotnet-runtime.zip' 'SHA512' $DotnetSha512
$dotnetDir = Join-Path $root 'dotnet'
[IO.Compression.ZipFile]::ExtractToDirectory($dotnetZip, $dotnetDir)
if (-not (Test-Path (Join-Path $dotnetDir "shared\Microsoft.NETCore.App\$DotnetVersion"))) {
    throw ".NET runtime $DotnetVersion did not extract where expected"
}
Remove-Item $nupkg, $dotnetZip

# signtool comes with the Windows SDK the VM already has. The x64 build, to
# match the x64 dlib; the newest SDK if several are installed.
$signtool = $env:SIGNTOOL_PATH
if (-not $signtool) {
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1 -ExpandProperty FullName
}
if (-not $signtool -or -not (Test-Path $signtool)) {
    throw 'signtool.exe (x64) not found - install the Windows SDK or set SIGNTOOL_PATH'
}
Write-Host "Using $signtool"

# The dlib authenticates through DefaultAzureCredential, which tries a chain
# of credentials. Everything but EnvironmentCredential (the three AZURE_*
# variables) is excluded: the chain ends in InteractiveBrowserCredential, and
# a SYSTEM process waiting on a browser that never opens is a hung build.
$metadata = [ordered]@{
    Endpoint               = $env:ARTIFACT_SIGNING_ENDPOINT
    CodeSigningAccountName = $env:ARTIFACT_SIGNING_ACCOUNT_NAME
    CertificateProfileName = $env:ARTIFACT_SIGNING_PROFILE_NAME
    ExcludeCredentials     = @(
        'ManagedIdentityCredential', 'WorkloadIdentityCredential', 'SharedTokenCacheCredential',
        'VisualStudioCredential', 'VisualStudioCodeCredential', 'AzureCliCredential',
        'AzurePowerShellCredential', 'AzureDeveloperCliCredential', 'InteractiveBrowserCredential'
    )
}
$metadataPath = Join-Path $root 'metadata.json'
$utf8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText($metadataPath, ($metadata | ConvertTo-Json), $utf8)

# Tauri runs this for every file it signs - the app binary, the MSI, the NSIS
# installer and (from inside makensis) the uninstaller - with %1 replaced by
# the path. Object form, so paths with spaces survive. beforeBuildCommand is
# blanked because the job builds the frontend in its own step.
$signScript = Join-Path $workspace 'scripts\windows-sign.ps1'
$tauriConfig = @{
    build  = @{ beforeBuildCommand = '' }
    bundle = @{ windows = @{ signCommand = @{
        cmd  = 'powershell'
        args = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $signScript, '%1')
    } } }
} | ConvertTo-Json -Depth 8
$tauriConfigPath = Join-Path $root 'tauri.signing.conf.json'
[IO.File]::WriteAllText($tauriConfigPath, $tauriConfig, $utf8)

# The job's temp directory moves into the workspace too. makensis writes the
# uninstaller to %TEMP% before signing it, and SYSTEM's own %TEMP% is under
# System32: the 32-bit makensis and PowerShell would see the SysWOW64 copy of
# that path while the x64 signtool opens the real one, and fail to find the
# file. makensis ignores the sign command's exit code for the uninstaller
# (Tauri emits `!uninstfinalize` without a compare), so that failure would be
# silent - hence the signing log, which "Verify signatures" reads to require
# that a file under this directory, i.e. the uninstaller, really was signed.
$tmpDir = Join-Path $root 'tmp'
New-Item -ItemType Directory -Path $tmpDir | Out-Null
$signLog = Join-Path $root 'signed.log'
$signOutput = Join-Path $root 'sign-output.log'

# $GITHUB_ENV is KEY=VALUE lines. Written without a BOM: PowerShell 5.1's
# utf8 encoding adds one, which would corrupt the first key.
$lines = @(
    "TRIPLE_C_SIGNTOOL=$signtool"
    "TRIPLE_C_SIGN_DLIB=$dlib"
    "TRIPLE_C_SIGN_METADATA=$metadataPath"
    "TRIPLE_C_SIGN_TIMESTAMP=$TimestampUrl"
    "DOTNET_ROOT=$dotnetDir"
    "DOTNET_ROOT_X64=$dotnetDir"
    "TRIPLE_C_TAURI_SIGN_CONFIG=$tauriConfigPath"
    "TRIPLE_C_SIGN_LOG=$signLog"
    "TRIPLE_C_SIGN_OUTPUT=$signOutput"
    "TRIPLE_C_SIGN_TMP=$tmpDir"
    "TEMP=$tmpDir"
    "TMP=$tmpDir"
)
[IO.File]::AppendAllText($env:GITHUB_ENV, (($lines -join "`n") + "`n"), $utf8)
Write-Host 'Code signing prepared.'
