# windows-sign.ps1 <file> - sign one file with Azure Artifact Signing.
#
# Tauri's bundle.windows.signCommand, set up by windows-signing-setup.ps1.
# Tauri calls it once per file it wants signed and fails the build on a
# non-zero exit - but shows none of this script's output when it does, so
# everything is also appended to $TRIPLE_C_SIGN_OUTPUT, which the workflow
# prints if the job fails.
#
# Only what ships is signed. Tauri also offers build-time tools - the WiX
# extension DLLs candle/light load, the NSIS plugins makensis embeds - and
# each signature is metered (about 1000 a month), so those are skipped. The
# allowlist below is the whole of what reaches users: the app binary, the MSI,
# the NSIS installer, and the uninstaller makensis writes to the job's temp
# directory. A file that already carries a valid signature is skipped too:
# Tauri presents the app binary once per bundle type.
#
# This may run as 32-bit PowerShell: the NSIS uninstaller is signed from inside
# makensis, which is 32-bit and resolves `powershell` to the SysWOW64 copy. So
# nothing here depends on $env:ProgramFiles or other per-bitness paths - every
# path comes in absolute from the setup script, and signtool is always the x64
# build, since that is what loads the x64 dlib.
#
# Credentials never touch a command line: the dlib reads AZURE_TENANT_ID,
# AZURE_CLIENT_ID and AZURE_CLIENT_SECRET from the environment itself.

param([Parameter(Mandatory = $true)][string]$Path)

$ErrorActionPreference = 'Stop'
$utf8 = New-Object System.Text.UTF8Encoding $false

function Write-Log([string]$Text) {
    Write-Host $Text
    if ($env:TRIPLE_C_SIGN_OUTPUT) {
        try { [IO.File]::AppendAllText($env:TRIPLE_C_SIGN_OUTPUT, "$Text`n", $utf8) } catch { }
    }
}

try {
    foreach ($name in 'TRIPLE_C_SIGNTOOL', 'TRIPLE_C_SIGN_DLIB', 'TRIPLE_C_SIGN_METADATA', 'TRIPLE_C_SIGN_TIMESTAMP',
                      'TRIPLE_C_SIGN_TMP', 'TRIPLE_C_SIGN_LOG', 'AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET') {
        if (-not [Environment]::GetEnvironmentVariable($name)) {
            throw "$name is not set - run windows-signing-setup.ps1 first and pass the AZURE_* secrets to this step"
        }
    }
    if (-not (Test-Path -LiteralPath $Path)) { throw "No such file to sign: $Path" }
    $full = (Resolve-Path -LiteralPath $Path).ProviderPath
    Write-Log "== $full"

    $ships = ($full -match '\\target\\release\\[^\\]+\.exe$') -or
             ($full -match '\\target\\release\\bundle\\(msi|nsis)\\[^\\]+\.(msi|exe)$') -or
             $full.StartsWith($env:TRIPLE_C_SIGN_TMP.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)
    if (-not $ships) {
        Write-Log 'skipped: build-time file, not shipped'
        exit 0
    }

    $existing = Get-AuthenticodeSignature -LiteralPath $full
    if ($existing.Status -eq 'Valid' -and $existing.TimeStamperCertificate) {
        Write-Log "skipped: already signed by $($existing.SignerCertificate.Subject)"
        [IO.File]::AppendAllText($env:TRIPLE_C_SIGN_LOG, "$full`n", $utf8)
        exit 0
    }

    # /d names the product in the UAC prompt, which for an MSI would otherwise
    # show a temporary file name. The timestamp is what keeps the signature
    # valid after the short-lived Artifact Signing certificate expires, so it
    # is not optional. /debug makes the dlib say why it failed, into the log.
    $arguments = @(
        'sign', '/v', '/debug',
        '/fd', 'SHA256',
        '/tr', $env:TRIPLE_C_SIGN_TIMESTAMP, '/td', 'SHA256',
        '/d', 'Triple-C',
        '/dlib', $env:TRIPLE_C_SIGN_DLIB,
        '/dmdf', $env:TRIPLE_C_SIGN_METADATA,
        $full
    )

    # Stop is relaxed around the call: Tauri captures this script's output,
    # and PowerShell 5.1 turns a native command's stderr into error records
    # when its own streams are redirected - under Stop, signtool's first
    # warning would kill the script before its exit code is read. Timestamp
    # servers and the signing endpoint fail transiently now and then, hence
    # the retries.
    $ErrorActionPreference = 'Continue'
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        & $env:TRIPLE_C_SIGNTOOL @arguments 2>&1 | ForEach-Object { Write-Log "$_" }
        $code = $LASTEXITCODE
        if ($code -eq 0) {
            # The evidence "Verify signatures" needs for the file it cannot see
            # afterwards - the NSIS uninstaller is embedded in the installer.
            [IO.File]::AppendAllText($env:TRIPLE_C_SIGN_LOG, "$full`n", $utf8)
            Write-Log 'signed'
            exit 0
        }
        Write-Log "signtool exited $code (attempt $attempt of 3)"
        if ($attempt -lt 3) { Start-Sleep -Seconds (10 * $attempt) }
    }
    exit 1
} catch {
    Write-Log "windows-sign.ps1 failed: $($_.Exception.Message)"
    exit 1
}
