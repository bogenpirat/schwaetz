# Sends input to a running schwaetz window as if typed into the active buffer
# (commands included), via WM_COPYDATA. Used for scripted UI checks and automation.
#
# Usage: .\scripts\send.ps1 [-ProcessId 1234] "/join #test" ["hello"] [...]
#        Special lines: "irc://host/#chan" opens a link, "!show" restores the window;
#        "!move x y", "!click x y", "!drag x0 y0 x1 y1", "!key <vk>", "!submit <text>" drive the UI (DIP coordinates).
#
# With several instances running (e.g. your own plus a test profile), -ProcessId is required so
# input never lands in the wrong one.

[CmdletBinding(PositionalBinding = $false)]
param([int]$ProcessId = 0, [Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Lines)
$ErrorActionPreference = 'Stop'
if (-not ('SchwaetzIpc2' -as [type])) {
    Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class SchwaetzIpc2 {
    [StructLayout(LayoutKind.Sequential)]
    struct COPYDATASTRUCT { public IntPtr dwData; public int cbData; public IntPtr lpData; }
    delegate bool EnumProc(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr lp);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, ref COPYDATASTRUCT lp);
    // Main windows of all running instances: (window, process id).
    public static List<KeyValuePair<IntPtr, uint>> Windows() {
        var found = new List<KeyValuePair<IntPtr, uint>>();
        EnumWindows((h, lp) => {
            var cls = new StringBuilder(64);
            GetClassNameW(h, cls, cls.Capacity);
            if (cls.ToString() == "schwaetz.main") { uint pid; GetWindowThreadProcessId(h, out pid); found.Add(new KeyValuePair<IntPtr, uint>(h, pid)); }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static void Send(IntPtr h, string text) {
        IntPtr buf = Marshal.StringToHGlobalUni(text);
        try {
            var cds = new COPYDATASTRUCT { dwData = (IntPtr)0x53574158, cbData = text.Length * 2, lpData = buf };
            SendMessageW(h, 0x004A, IntPtr.Zero, ref cds);
        } finally { Marshal.FreeHGlobal(buf); }
    }
}
'@
}
$windows = [SchwaetzIpc2]::Windows()
if ($ProcessId) { $windows = @($windows | Where-Object { $_.Value -eq $ProcessId }) }
if ($windows.Count -eq 0) { throw 'schwaetz is not running' + $(if ($ProcessId) { " (process $ProcessId)" } else { '' }) }
if ($windows.Count -gt 1) {
    $pids = ($windows | ForEach-Object { $_.Value }) -join ', '
    throw "Several schwaetz instances are running (processes $pids); pass -ProcessId."
}
[SchwaetzIpc2]::Send($windows[0].Key, ($Lines -join "`n"))
