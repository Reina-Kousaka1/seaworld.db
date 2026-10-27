$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$buildRoot = Join-Path $repoRoot 'build'
$classesPath = Join-Path $buildRoot 'classes'

if (-not (Test-Path -LiteralPath $classesPath)) {
    New-Item -ItemType Directory -Path $classesPath -Force | Out-Null
}

$resolvedBuildRoot = (Resolve-Path -LiteralPath $buildRoot).Path
$resolvedClassesPath = (Resolve-Path -LiteralPath $classesPath).Path
$repoPrefix = $repoRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) +
    [IO.Path]::DirectorySeparatorChar
$buildPrefix = $resolvedBuildRoot.TrimEnd([IO.Path]::DirectorySeparatorChar,
    [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $resolvedBuildRoot.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Resolved build directory is outside the repository: $resolvedBuildRoot"
}
if (-not $resolvedClassesPath.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Resolved classes directory is outside the build directory: $resolvedClassesPath"
}
if (Test-Path -LiteralPath $classesPath) {
    Remove-Item -LiteralPath $classesPath -Recurse -Force
}
New-Item -ItemType Directory -Path $classesPath -Force | Out-Null

$sources = @(Get-ChildItem -LiteralPath (Join-Path $repoRoot 'src') -Recurse -Filter '*.java' |
    Sort-Object FullName |
    ForEach-Object { $_.FullName })
if ($sources.Count -eq 0) {
    throw 'No Java sources were found.'
}

& javac --release 21 -Xlint:all -Werror -d $classesPath @sources
if ($LASTEXITCODE -ne 0) {
    throw "javac failed with exit code $LASTEXITCODE"
}

& java -ea -cp $classesPath org.seaworlddb.EngineTests
if ($LASTEXITCODE -ne 0) {
    throw "Engine tests failed with exit code $LASTEXITCODE"
}
