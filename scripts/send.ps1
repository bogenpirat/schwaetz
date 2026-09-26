# Sends input to the running schwaetz window as if typed into the active buffer
# (commands included), via WM_COPYDATA. Used for scripted UI checks and automation.
#
# Usage: .\scripts\send.ps1 "/join #test" ["hello"] [...]
#        Special lines: "irc://host/#chan" opens a link, "!show" restores the window;
#        "!move x y", "!click x y", "!drag x0 y0 x1 y1", "!key <vk>", "!submit <text>" drive the UI (DIP coordinates).

param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Lines)
$ErrorActionPreference = 'Stop'
if (-not ('SchwaetzIpc' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class SchwaetzIpc {
    [StructLayout(LayoutKind.Sequential)]
    struct COPYDATASTRUCT { public IntPtr dwData; public int cbData; public IntPtr lpData; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, ref COPYDATASTRUCT lp);
    public static bool Send(string text) {
        IntPtr h = FindWindowW("schwaetz.main", null);
        if (h == IntPtr.Zero) return false;
        IntPtr buf = Marshal.StringToHGlobalUni(text);
        try {
            var cds = new COPYDATASTRUCT { dwData = (IntPtr)0x53574158, cbData = text.Length * 2, lpData = buf };
            SendMessageW(h, 0x004A, IntPtr.Zero, ref cds);
        } finally { Marshal.FreeHGlobal(buf); }
        return true;
    }
}
'@
}
if (-not [SchwaetzIpc]::Send(($Lines -join "`n"))) { throw 'schwaetz is not running' }
