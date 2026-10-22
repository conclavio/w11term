[CmdletBinding()]
param([Parameter(Mandatory,Position=0)][string]$Path)
$ErrorActionPreference = 'Stop'
$rows = @(Import-Csv -LiteralPath $Path)
if ($rows.Count -eq 0) { throw 'No samples in CSV' }
foreach ($column in $rows[0].PSObject.Properties.Name) {
    $samples = @($rows | ForEach-Object {
        $text = $_.$column
        if (-not [string]::IsNullOrWhiteSpace($text)) {
            $sample = [double]::Parse($text, [Globalization.CultureInfo]::InvariantCulture)
            if ([double]::IsNaN($sample) -or [double]::IsInfinity($sample) -or $sample -lt 0) {
                throw "Invalid latency in column $column"
            }
            $sample
        }
    } | Sort-Object)
    if ($samples.Count -eq 0) { continue }
    $count = $samples.Count
    $middle = [int][Math]::Floor($count / 2)
    $median = if ($count % 2) { $samples[$middle] } else { ($samples[$middle - 1] + $samples[$middle]) / 2 }
    [pscustomobject]@{
        Measurement = $column
        Samples = $count
        MedianMs = [Math]::Round($median, 2)
        P95Ms = $samples[[int][Math]::Ceiling($count * 0.95) - 1]
        P99Ms = $samples[[int][Math]::Ceiling($count * 0.99) - 1]
        MeanMs = [Math]::Round(($samples | Measure-Object -Average).Average, 2)
        MaxMs = $samples[-1]
    }
}
