param(
    [Parameter(Mandatory = $true)][string]$MetadataPath,
    [Parameter(Mandatory = $true)][string]$DumperAssembly,
    [Parameter(Mandatory = $true)][string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$metadataFile = (Resolve-Path -LiteralPath $MetadataPath).Path
$dumperFile = (Resolve-Path -LiteralPath $DumperAssembly).Path
$outputFile = [IO.Path]::GetFullPath($OutputPath)
if ($outputFile -in @($metadataFile, $dumperFile)) {
    throw 'Output must differ from both inputs.'
}
$assembly = [Reflection.Assembly]::LoadFrom($dumperFile)
$bytes = [IO.File]::ReadAllBytes($metadataFile)
# The parser requires a writable stream. Only this private in-memory copy is writable.
$stream = [IO.MemoryStream]::new($bytes, $true)
try {
    $metadata = $assembly.GetType('Il2CppDumper.Metadata').GetConstructors()[0].Invoke([object[]]@($stream))
    $types = [Collections.Generic.List[object]]::new()
    $methodsSeen = 0
    $fieldsSeen = 0
    for ($typeIndex = 0; $typeIndex -lt $metadata.typeDefs.Count; $typeIndex++) {
        $type = $metadata.typeDefs[$typeIndex]
        if ($type.method_count -gt 0 -and ($type.methodStart -lt 0 -or $type.methodStart + $type.method_count -gt $metadata.methodDefs.Count)) {
            throw "Invalid method range for type $typeIndex"
        }
        if ($type.field_count -gt 0 -and ($type.fieldStart -lt 0 -or $type.fieldStart + $type.field_count -gt $metadata.fieldDefs.Count)) {
            throw "Invalid field range for type $typeIndex"
        }
        $methods = [Collections.Generic.List[object]]::new()
        for ($i = 0; $i -lt $type.method_count; $i++) {
            $method = $metadata.methodDefs[$type.methodStart + $i]
            if ($method.declaringType -ne $typeIndex) { throw "Method owner mismatch for type $typeIndex" }
            $methods.Add([ordered]@{
                name = $metadata.GetStringFromIndex($method.nameIndex)
                argc = [int]$method.parameterCount
                token = [long]$method.token
            })
            $methodsSeen++
        }
        $fields = [Collections.Generic.List[string]]::new()
        for ($i = 0; $i -lt $type.field_count; $i++) {
            $fields.Add($metadata.GetStringFromIndex($metadata.fieldDefs[$type.fieldStart + $i].nameIndex))
            $fieldsSeen++
        }
        $types.Add([ordered]@{
            index = $typeIndex
            namespace = $metadata.GetStringFromIndex($type.namespaceIndex)
            name = $metadata.GetStringFromIndex($type.nameIndex)
            methods = $methods.ToArray()
            fields = $fields.ToArray()
        })
    }
    if ($methodsSeen -ne $metadata.methodDefs.Count -or $fieldsSeen -ne $metadata.fieldDefs.Count) {
        throw 'Declared member coverage does not equal metadata totals.'
    }
    $result = [ordered]@{
        schema = 1
        metadata_path = $metadataFile
        metadata_sha256 = (Get-FileHash -LiteralPath $metadataFile -Algorithm SHA256).Hash.ToLowerInvariant()
        parser_sha256 = (Get-FileHash -LiteralPath $dumperFile -Algorithm SHA256).Hash.ToLowerInvariant()
        version = $metadata.Version
        counts = [ordered]@{ types = $types.Count; methods = $methodsSeen; fields = $fieldsSeen }
        limits = @('Declarations only; no method bodies, runtime calls, remote implementation, or server acceptance are established.')
        types = $types.ToArray()
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputFile)) | Out-Null
    [IO.File]::WriteAllText($outputFile, ($result | ConvertTo-Json -Depth 10 -Compress), [Text.UTF8Encoding]::new($false))
    $result.counts | ConvertTo-Json -Compress
} finally {
    $stream.Dispose()
}
