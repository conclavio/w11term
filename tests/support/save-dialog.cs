using System;
using System.Collections.Generic;
using System.IO;
using Microsoft.Win32.SafeHandles;
using System.Runtime.InteropServices;
using System.Text;

// The common file dialog mixes Shell UIA elements and standard HWND controls.
// Query only the fixture's windows and the children of its verified modal dialog.
public static class FixtureDialog {
    private delegate bool Visitor(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumWindows(Visitor visit, IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr owner, Visitor visit, IntPtr data);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassName(IntPtr hwnd, StringBuilder name, int size);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, IntPtr size, StringBuilder text, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, IntPtr size, string text, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll")] private static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr hwnd, uint command);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern uint GetFinalPathNameByHandle(SafeFileHandle file, StringBuilder path, uint size, uint flags);

    private static string DiskPath(string path) {
        using (var file = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
            var result = new StringBuilder(32768);
            uint count = GetFinalPathNameByHandle(file.SafeFileHandle, result, (uint)result.Capacity, 0);
            if (count == 0 || count >= result.Capacity) throw new IOException("Cannot resolve saved path: " + path);
            return result.ToString();
        }
    }

    public static bool SameFile(string selected, string reported) {
        if (!File.Exists(selected) || !File.Exists(reported)) return false;
        return String.Equals(DiskPath(selected), DiskPath(reported), StringComparison.OrdinalIgnoreCase);
    }

    public static IntPtr[] Windows(uint process) {
        var result = new List<IntPtr>();
        EnumWindows((hwnd, _) => {
            uint pid;
            GetWindowThreadProcessId(hwnd, out pid);
            if (pid == process && IsWindowVisible(hwnd)) result.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }

    private static bool IsControl(IntPtr hwnd, string type) {
        var name = new StringBuilder(128);
        GetClassName(hwnd, name, name.Capacity);
        return IsWindowVisible(hwnd) && IsWindowEnabled(hwnd) && name.ToString() == type;
    }

    public static bool SetFilename(IntPtr dialog, string path) {
        bool changed = false;
        EnumChildWindows(dialog, (hwnd, _) => {
            if (!IsControl(hwnd, "Edit")) return true;
            var text = new StringBuilder(256);
            IntPtr result;
            SendMessageTimeout(hwnd, 0x000D, (IntPtr)text.Capacity, text, 2, 1000, out result); // WM_GETTEXT
            if (text.ToString() != "terminal.txt" && text.ToString() != "terminal") return true;
            changed = SendMessageTimeout(hwnd, 0x000C, IntPtr.Zero, path, 2, 1000, out result) != IntPtr.Zero
                && result != IntPtr.Zero; // WM_SETTEXT
            return !changed;
        }, IntPtr.Zero);
        return changed;
    }

    public static bool ClickButton(IntPtr dialog, int id) {
        bool clicked = false;
        EnumChildWindows(dialog, (hwnd, _) => {
            if (GetDlgCtrlID(hwnd) != id || !IsControl(hwnd, "Button")) return true;
            // BM_CLICK can be ignored on an inactive desktop. Post the enabled
            // button's BN_CLICKED notification to its parent. WM_COMMAND still
            // runs the dialog's own validation and overwrite confirmation.
            clicked = PostMessage(GetParent(hwnd), 0x0111, (IntPtr)id, hwnd);
            return !clicked;
        }, IntPtr.Zero);
        return clicked;
    }

    public static bool ConfirmReplace(IntPtr dialog) {
        // The shell can render a Task Dialog instead of a standard HWND button.
        return PostMessage(dialog, 0x0400 + 102, (IntPtr)6, IntPtr.Zero); // TDM_CLICK_BUTTON / IDYES
    }
}
