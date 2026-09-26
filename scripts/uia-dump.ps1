# Dumps the UI Automation tree of a running schwaetz window (accessibility check).
# Optionally sets the input text and selects a buffer through UIA patterns.
#
# Usage: .\scripts\uia-dump.ps1 [-SetInput "text"] [-Select "#channel"]

param([string]$SetInput = "", [string]$Select = "")
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$p = Get-Process schwaetz | Select-Object -First 1
$root = [System.Windows.Automation.AutomationElement]::FromHandle($p.MainWindowHandle)
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker

function Dump($el, $depth) {
    if ($depth -gt 3) { return }
    $c = $el.Current
    $pad = '  ' * $depth
    Write-Output ("{0}{1} '{2}' [{3}]" -f $pad, $c.ControlType.ProgrammaticName.Replace('ControlType.', ''), $c.Name, $c.AutomationId)
    $child = $walker.GetFirstChild($el)
    $n = 0
    while ($child -ne $null -and $n -lt 12) {
        Dump $child ($depth + 1)
        $child = $walker.GetNextSibling($child)
        $n++
    }
}
Dump $root 0

if ($SetInput) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'input')
    $input = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    $vp = $input.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $vp.SetValue($SetInput)
    Write-Output "input value now: '$($vp.Current.Value)'"
}
if ($Select) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'buffer')
    $items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
    foreach ($it in $items) {
        if ($it.Current.Name.StartsWith($Select)) {
            $it.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
            Write-Output "selected '$($it.Current.Name)'"
            break
        }
    }
}
