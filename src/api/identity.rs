//! How jellydl presents itself to Jellyfin (the `Authorization: MediaBrowser ...` header),
//! and the saved login session.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::IdentityConfig;

/// Values Jellyfin needs in the `Authorization` header. It uses them to track the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
}

impl Identity {
    /// Client and device "jellydl" (no hostname is sent), and a device id that is
    /// generated once and saved in `data_dir`.
    ///
    /// Each value set in the `[identity]` config section replaces its default.
    /// A forced `device_id` is not written to the data folder.
    pub fn new(data_dir: &Path, ov: &IdentityConfig) -> Self {
        Self {
            client: ov.client.clone().unwrap_or_else(|| "jellydl".into()),
            device: ov.device.clone().unwrap_or_else(|| "jellydl".into()),
            device_id: ov.device_id.clone().unwrap_or_else(|| {
                load_or_create(&data_dir.join("device_id"), || {
                    uuid::Uuid::new_v4().simple().to_string()
                })
            }),
            version: ov
                .version
                .clone()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
        }
    }

    /// The `Authorization` header value.
    pub fn header(&self, token: Option<&str>) -> String {
        let q = |s: &str| s.replace('"', "");
        let mut h = format!(
            r#"MediaBrowser Client="{}", Device="{}", DeviceId="{}", Version="{}""#,
            q(&self.client),
            q(&self.device),
            q(&self.device_id),
            q(&self.version),
        );
        if let Some(t) = token {
            h.push_str(&format!(r#", Token="{}""#, q(t)));
        }
        h
    }
}

fn load_or_create(file: &Path, make: impl FnOnce() -> String) -> String {
    if let Ok(s) = std::fs::read_to_string(file) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let id = make();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, &id);
    id
}

/// A saved login, used again at the next start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub server_url: String,
    pub username: String,
    pub device_id: String,
    pub token: String,
    pub user_id: String,
    pub user_name: String,
}

pub fn load_session(file: &Path) -> Option<Session> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// Save the session. The file is readable by the owner only: it holds a token.
pub fn save_session(file: &Path, s: &Session) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(file)?;
    f.write_all(&serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

pub fn forget_session(file: &Path) {
    let _ = std::fs::remove_file(file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_format() {
        let id = Identity {
            client: "jellydl".into(),
            device: "my\"host".into(),
            device_id: "abc".into(),
            version: "0.1.0".into(),
        };
        assert_eq!(
            id.header(Some("tok")),
            r#"MediaBrowser Client="jellydl", Device="myhost", DeviceId="abc", Version="0.1.0", Token="tok""#
        );
    }

    #[test]
    fn overrides_replace_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let ov = IdentityConfig {
            device: Some("tower".into()),
            device_id: Some("fixed-id".into()),
            ..Default::default()
        };
        let id = Identity::new(dir.path(), &ov);
        assert_eq!(id.client, "jellydl");
        assert_eq!(id.device, "tower");
        assert_eq!(id.device_id, "fixed-id");
        assert_eq!(id.version, env!("CARGO_PKG_VERSION"));
        assert!(
            !dir.path().join("device_id").exists(),
            "a forced id is not saved"
        );
    }

    #[test]
    fn device_id_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::new(dir.path(), &IdentityConfig::default());
        let b = Identity::new(dir.path(), &IdentityConfig::default());
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.client, "jellydl");
        assert_eq!(a.device, "jellydl");
    }

    #[test]
    fn session_round_trip_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.json");
        let s = Session {
            server_url: "http://a".into(),
            username: "u".into(),
            device_id: "d".into(),
            token: "t".into(),
            user_id: "1".into(),
            user_name: "U".into(),
        };
        save_session(&f, &s).unwrap();
        assert_eq!(load_session(&f), Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
