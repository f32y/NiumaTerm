#Requires -Version 5.1
<#
.SYNOPSIS
Redraw the effort pill's gauge faces in assets/icons.

.DESCRIPTION
The composer's effort control shows a dial pointing at the level the session
stands on. A dial cannot be drawn from one asset and rotated, because gpui
renders an icon as a monochrome mask with no transform of its own, so each
position is a file:

    assets/icons/effort-gauge-0.svg   empty face, level not reported
    assets/icons/effort-gauge-1.svg   cheapest level
    ...
    assets/icons/effort-gauge-6.svg   dearest level, arc full

Six filled faces is what the longest ladder any harness offers needs. The
mapping from a level to a face lives in `effort_gauge_step`, in
crates/app_agent/src/view/settings_row.rs; this script owns only the drawing.

Each face is a half-circle track at low alpha, an arc over it filled in
proportion to the level, and a needle from the dial's centre to the end of that
arc. The filled arc rather than the needle alone is what carries the reading:
at the 12px the pill draws these, one step of needle rotation moves the tip by
about two pixels, which is not a difference the eye picks up, while a change in
arc length is.

Run it after changing any of the geometry below; it overwrites every face and
leaves the working tree clean when nothing about them changed.

    ./scripts/generate-effort-gauge-icons.ps1
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Faces past the empty one, and the icon geometry in the 24x24 viewBox the
# project's other icons are drawn in. The dial sits below centre because its
# needle takes the space above it, so the drawn mark still reads as centred.
$Steps = 6
$CentreX = 12.0
$CentreY = 15.5
$Radius = 7.0
$TrackWidth = 2.0
$NeedleLength = 5.2
$NeedleWidth = 1.6

$Culture = [cultureinfo]::InvariantCulture
$IconDirectory = Join-Path $PSScriptRoot '..\assets\icons'

# Trailing zeros carry no meaning in path data and only make the diff noisier
# when a value happens to land on a whole number.
function Format-Coordinate([double] $value) {
    [string]::Format($Culture, '{0:0.###}', $value)
}

function Format-Length([double] $value) {
    [string]::Format($Culture, '{0:0.00}', $value)
}

# The track, swept anticlockwise from the dial's left end to its right one. Both
# the track and the filled arc are drawn on this same path; what separates them
# is the dash pattern.
$arc = 'M{0} {1} A{2} {2} 0 0 1 {3} {1}' -f `
    (Format-Coordinate ($CentreX - $Radius)),
    (Format-Coordinate $CentreY),
    (Format-Coordinate $Radius),
    (Format-Coordinate ($CentreX + $Radius))

$sweep = [Math]::PI * $Radius

for ($step = 0; $step -le $Steps; $step++) {
    $fraction = $step / $Steps

    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">')
    $lines.Add(('  <path d="{0}" stroke="currentColor" stroke-width="{1}" stroke-linecap="round" stroke-opacity="0.3"/>' -f `
        $arc, (Format-Coordinate $TrackWidth)))

    # The empty face is the one a session with no reported level is drawn with,
    # so it carries the track alone: no arc to read and no needle to point.
    if ($step -gt 0) {
        $lines.Add(('  <path d="{0}" stroke="currentColor" stroke-width="{1}" stroke-linecap="round" stroke-dasharray="{2} {3}"/>' -f `
            $arc,
            (Format-Coordinate $TrackWidth),
            (Format-Length ($sweep * $fraction)),
            (Format-Length $sweep)))

        # The needle ends on the dial rather than short of it, so its tip and
        # the end of the filled arc are the same point and the two marks read
        # as one reading rather than as two.
        $angle = [Math]::PI * (1.0 - $fraction)
        $tipX = $CentreX + $NeedleLength * [Math]::Cos($angle)
        $tipY = $CentreY - $NeedleLength * [Math]::Sin($angle)

        $lines.Add(('  <path d="M{0} {1} L{2} {3}" stroke="currentColor" stroke-width="{4}" stroke-linecap="round"/>' -f `
            (Format-Coordinate $CentreX),
            (Format-Coordinate $CentreY),
            (Format-Length $tipX),
            (Format-Length $tipY),
            (Format-Coordinate $NeedleWidth)))
    }

    $lines.Add('</svg>')

    # Written as bytes rather than through Set-Content: the repository stores
    # these with LF endings and no byte-order mark, and PowerShell's own text
    # writers supply both of those from the host's defaults instead.
    $path = Join-Path $IconDirectory "effort-gauge-$step.svg"
    $text = ($lines -join "`n") + "`n"
    [System.IO.File]::WriteAllBytes($path, [System.Text.Encoding]::UTF8.GetBytes($text))

    Write-Host "wrote $path"
}
