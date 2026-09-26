//! File locations. Portable mode (a `portable.txt` next to the executable) keeps everything in a
//! `data` folder beside the exe; otherwise config lives in `%APPDATA%\schwaetz` and caches in
//! `%LOCALAPPDATA%\schwaetz`.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub portable: bool,
}

impl Paths {
    pub fn detect() -> Paths {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
        if let Some(dir) = exe_dir.filter(|d| d.join("portable.txt").exists()) {
            let data = dir.join("data");
            return Paths {
                config_dir: data.clone(),
                data_dir: data.clone(),
                cache_dir: data.join("cache"),
                portable: true,
            };
        }
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| roaming.clone());
        Paths {
            config_dir: roaming.join("schwaetz"),
            data_dir: local.join("schwaetz"),
            cache_dir: local.join("schwaetz").join("cache"),
            portable: false,
        }
    }

    /// Paths rooted in one directory (tests, `--profile <dir>`).
    pub fn in_dir(dir: impl Into<PathBuf>) -> Paths {
        let d = dir.into();
        Paths { config_dir: d.clone(), data_dir: d.clone(), cache_dir: d.join("cache"), portable: true }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    pub fn scripts_dir(&self) -> PathBuf {
        self.config_dir.join("scripts")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn history_db(&self) -> PathBuf {
        self.data_dir.join("history.sqlite")
    }

    pub fn session_file(&self) -> PathBuf {
        self.data_dir.join("session.toml")
    }

    pub fn crash_dir(&self) -> PathBuf {
        self.data_dir.join("crashes")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.config_dir, &self.data_dir, &self.cache_dir] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}
