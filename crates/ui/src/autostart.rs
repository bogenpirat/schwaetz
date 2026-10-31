//! "Start with Windows": a value in the user's `Run` key. The registry is the only record of it
//! (there is no setting in the config file), so the switch stays true when the entry is changed
//! elsewhere, such as in Task Manager's list of startup apps.
//!
//! The switch is on only if the entry starts this very executable: another build's entry shows
//! as off, and switching on takes the entry over.

use schwaetz_core::Paths;
use std::path::Path;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_BINARY,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
};
use windows::core::HSTRING;

/// Where Windows keeps the startup entries and whether each is allowed to run.
struct Keys<'a> {
    run: &'a str,
    /// Task Manager's on/off state of the entries in `run`.
    approved: &'a str,
}

const USER_KEYS: Keys = Keys {
    run: r"Software\Microsoft\Windows\CurrentVersion\Run",
    approved: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
};

const VALUE_NAME: &str = "schwaetz";

/// Whether this executable (with this profile) starts with Windows.
pub fn enabled(paths: &Paths) -> bool {
    command(paths).is_ok_and(|c| is_on(&USER_KEYS, &c))
}

/// Makes this executable (with this profile) start with Windows, or removes the entry.
pub fn set(paths: &Paths, on: bool) -> Result<(), String> {
    let result = if on { switch_on(&USER_KEYS, &command(paths)?) } else { switch_off(&USER_KEYS) };
    result.map_err(|e| e.message())
}

fn command(paths: &Paths) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let profile = paths.profile.as_deref().map(std::path::absolute).transpose().map_err(|e| e.to_string())?;
    Ok(command_line(&exe, profile.as_deref()))
}

/// The command to start `exe` with, as Windows will parse it.
fn command_line(exe: &Path, profile: Option<&Path>) -> String {
    let exe = quote(exe);
    match profile {
        Some(dir) => format!("{exe} --profile {}", quote(dir)),
        None => exe,
    }
}

/// Backslashes right before the closing quote have to be doubled, or they would escape it.
fn quote(path: &Path) -> String {
    let s = path.display().to_string();
    let trailing = s.len() - s.trim_end_matches('\\').len();
    format!("\"{s}{}\"", "\\".repeat(trailing))
}

fn is_on(keys: &Keys, command: &str) -> bool {
    let matches = read_string(keys.run).is_some_and(|c| c.eq_ignore_ascii_case(command));
    matches && !switched_off_in_task_manager(keys)
}

/// Task Manager marks a disabled entry by setting the lowest bit of the first byte of its value.
fn switched_off_in_task_manager(keys: &Keys) -> bool {
    let mut flags = [0u8; 12];
    let mut size = flags.len() as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(keys.approved),
            &HSTRING::from(VALUE_NAME),
            RRF_RT_REG_BINARY,
            None,
            Some(flags.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    read.is_ok() && flags[0] & 1 == 1
}

fn read_string(key: &str) -> Option<String> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(VALUE_NAME));
    let mut size = 0u32;
    let query = unsafe { RegGetValueW(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    query.ok().ok()?;
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    read.ok().ok()?;
    let end = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Ticking the switch is an explicit wish, so it also lifts a block from Task Manager.
fn switch_on(keys: &Keys, command: &str) -> windows::core::Result<()> {
    write_string(keys.run, command)?;
    delete_value(keys.approved)
}

fn switch_off(keys: &Keys) -> windows::core::Result<()> {
    delete_value(keys.run)?;
    delete_value(keys.approved)
}

fn write_string(key: &str, value: &str) -> windows::core::Result<()> {
    let data: Vec<u8> = crate::win::wide(value).iter().flat_map(|u| u.to_le_bytes()).collect();
    write_value(key, REG_SZ, &data)
}

fn write_value(key: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> windows::core::Result<()> {
    unsafe {
        let mut handle = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
        .ok()?;
        let written = RegSetValueExW(handle, &HSTRING::from(VALUE_NAME), None, kind, Some(data));
        let _ = RegCloseKey(handle);
        written.ok()
    }
}

/// Deleting what is not there counts as done.
fn delete_value(key: &str) -> windows::core::Result<()> {
    let deleted = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(VALUE_NAME)) };
    match deleted {
        WIN32_ERROR(code) if code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0 => Ok(()),
        other => other.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use windows::Win32::System::Registry::{REG_BINARY, RegDeleteTreeW};

    /// A throwaway registry key standing in for Windows' own: tests never touch the real entry.
    struct TestKeys {
        root: String,
        run: String,
        approved: String,
    }

    impl TestKeys {
        fn new(name: &str) -> TestKeys {
            let root = format!(r"Software\schwaetz-test-{}-{name}", std::process::id());
            TestKeys { run: format!(r"{root}\Run"), approved: format!(r"{root}\Approved"), root }
        }

        fn keys(&self) -> Keys<'_> {
            Keys { run: &self.run, approved: &self.approved }
        }

        /// Writes the state Task Manager leaves behind when it disables an entry.
        fn disable_in_task_manager(&self) {
            write_value(&self.approved, REG_BINARY, &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        }
    }

    impl Drop for TestKeys {
        fn drop(&mut self) {
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(self.root.as_str())) };
        }
    }

    #[test]
    fn switching_on_and_off() {
        let test = TestKeys::new("switch");
        let keys = test.keys();
        assert!(!is_on(&keys, "app"), "nothing there yet");
        switch_off(&keys).expect("switching off what is not there is fine");

        switch_on(&keys, "\"C:\\schwätz\\app.exe\"").unwrap();
        assert!(is_on(&keys, "\"C:\\schwätz\\app.exe\""), "the value survives the round trip");
        assert!(is_on(&keys, "\"c:\\schwätz\\APP.exe\""), "paths compare without regard to case");

        switch_off(&keys).unwrap();
        assert!(!is_on(&keys, "\"C:\\schwätz\\app.exe\""));
    }

    #[test]
    fn another_builds_entry_counts_as_off_until_taken_over() {
        let test = TestKeys::new("other");
        let keys = test.keys();
        switch_on(&keys, "debug.exe").unwrap();
        assert!(!is_on(&keys, "release.exe"));
        switch_on(&keys, "release.exe").unwrap();
        assert!(is_on(&keys, "release.exe"));
        assert!(!is_on(&keys, "debug.exe"));
    }

    #[test]
    fn task_manager_block_shows_as_off_and_switching_on_lifts_it() {
        let test = TestKeys::new("approved");
        let keys = test.keys();
        switch_on(&keys, "app").unwrap();
        test.disable_in_task_manager();
        assert!(!is_on(&keys, "app"));
        switch_on(&keys, "app").unwrap();
        assert!(is_on(&keys, "app"));
    }

    #[test]
    fn command_lines() {
        let exe = PathBuf::from(r"C:\Program Files\schwätz\schwaetz.exe");
        assert_eq!(command_line(&exe, None), r#""C:\Program Files\schwätz\schwaetz.exe""#);
        assert_eq!(
            command_line(&exe, Some(Path::new(r"D:\my profile"))),
            r#""C:\Program Files\schwätz\schwaetz.exe" --profile "D:\my profile""#
        );
        assert_eq!(
            command_line(&exe, Some(Path::new("D:\\"))),
            r#""C:\Program Files\schwätz\schwaetz.exe" --profile "D:\\""#,
            "a trailing backslash must not escape the closing quote"
        );
    }
}
