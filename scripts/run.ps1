# Start OpenConverter on Windows (PowerShell 5.1+). Extra arguments are passed through,
# for example:  .\scripts\run.ps1 --port 9000 --no-browser
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$python = $null
if (Get-Command py -ErrorAction SilentlyContinue) { $python = @("py", "-3") }
elseif (Get-Command python -ErrorAction SilentlyContinue) { $python = @("python") }
else { Write-Error "Python 3.12 or newer is required: https://www.python.org/downloads/"; exit 1 }
$exe = $python[0]
$pre = @()
if ($python.Length -gt 1) { $pre = $python[1..($python.Length - 1)] }
& $exe @pre -m openconverter serve @args
exit $LASTEXITCODE
