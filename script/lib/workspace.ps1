
function ParseMomorWorkspace {
    $metadata = cargo metadata --no-deps --offline | ConvertFrom-Json
    $env:MOMOR_WORKSPACE = $metadata.workspace_root
    $env:RELEASE_VERSION = $metadata.packages | Where-Object { $_.name -eq "momor" } | Select-Object -ExpandProperty version
}
