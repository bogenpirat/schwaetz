//! Passwords and tokens in the Windows Credential Manager (generic credentials named
//! `schwaetz:<network>:<kind>`), so they never touch the config file.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Sasl,
    ServerPassword,
    TwitchToken,
    /// Twitch Helix API OAuth token (live checks).
    TwitchApi,
}

impl SecretKind {
    fn as_str(self) -> &'static str {
        match self {
            SecretKind::Sasl => "sasl",
            SecretKind::ServerPassword => "pass",
            SecretKind::TwitchToken => "twitch",
            SecretKind::TwitchApi => "twitch-api",
        }
    }
}

fn target(network: &str, kind: SecretKind) -> String {
    format!("schwaetz:{}:{}", network.to_lowercase(), kind.as_str())
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn get(target: &str) -> Option<String> {
        let t = wide(target);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated target; CredReadW allocates `cred`, freed with CredFree.
        unsafe {
            if CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                return None;
            }
            let c = &*cred;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let s = String::from_utf8(bytes.to_vec()).ok();
            CredFree(cred as *const _);
            s
        }
    }

    pub fn set(target: &str, user: &str, secret: &str) -> bool {
        let t = wide(target);
        let u = wide(user);
        let blob = secret.as_bytes();
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: t.as_ptr() as *mut u16,
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: u.as_ptr() as *mut u16,
        };
        // SAFETY: all pointers are valid for the duration of the call.
        unsafe { CredWriteW(&cred, 0) != 0 }
    }

    pub fn delete(target: &str) -> bool {
        let t = wide(target);
        // SAFETY: valid NUL-terminated target.
        unsafe { CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0) != 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static STORE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    pub fn get(target: &str) -> Option<String> {
        STORE.lock().unwrap().as_ref()?.get(target).cloned()
    }
    pub fn set(target: &str, _user: &str, secret: &str) -> bool {
        STORE.lock().unwrap().get_or_insert_with(HashMap::new).insert(target.into(), secret.into());
        true
    }
    pub fn delete(target: &str) -> bool {
        STORE.lock().unwrap().as_mut().is_some_and(|m| m.remove(target).is_some())
    }
}

pub fn get(network: &str, kind: SecretKind) -> Option<String> {
    imp::get(&target(network, kind))
}

pub fn set(network: &str, kind: SecretKind, secret: &str) -> bool {
    imp::set(&target(network, kind), "schwaetz", secret)
}

pub fn delete(network: &str, kind: SecretKind) -> bool {
    imp::delete(&target(network, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let net = format!("test-{}", std::process::id());
        assert!(set(&net, SecretKind::Sasl, "hunter2 ✓"));
        assert_eq!(get(&net, SecretKind::Sasl).as_deref(), Some("hunter2 ✓"));
        assert!(delete(&net, SecretKind::Sasl));
        assert_eq!(get(&net, SecretKind::Sasl), None);
    }
}
