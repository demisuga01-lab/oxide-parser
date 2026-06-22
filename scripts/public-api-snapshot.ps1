param(
    [string]$Package = "oxide-engine",
    [string]$Output = "docs/public-api-oxide-engine.txt"
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command cargo-public-api -ErrorAction SilentlyContinue) -and
    -not (cargo --list | Select-String -Quiet "^    public-api")) {
    throw "cargo-public-api is not installed. Run: cargo install cargo-public-api"
}

cargo public-api -p $Package --simplified | Set-Content -Encoding utf8NoBOM $Output
Write-Host "Wrote $Output"
