use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::WhitelistConfig;

#[derive(Debug, Clone)]
pub struct AccessControl {
    allowed_user_ids: HashSet<i64>,
    allowed_chat_ids: HashSet<i64>,
    allowed_roots: Vec<PathBuf>,
}

impl AccessControl {
    pub fn from_config(cfg: &WhitelistConfig, base_dir: &Path) -> Result<Self> {
        let mut roots: Vec<PathBuf> = cfg
            .allowed_folders
            .iter()
            .map(|folder| absolutize(folder, base_dir))
            .collect();

        if let Some(file) = &cfg.allowed_folders_file {
            let loaded = load_allowed_folders_file(file, base_dir)?;
            roots.extend(loaded);
        }

        let roots = dedup_paths(roots);

        Ok(Self {
            allowed_user_ids: cfg.allowed_user_ids.iter().copied().collect(),
            allowed_chat_ids: cfg.allowed_chat_ids.iter().copied().collect(),
            allowed_roots: roots,
        })
    }

    pub fn is_user_allowed(&self, user_id: Option<i64>, chat_id: i64) -> bool {
        let user_ok = self.allowed_user_ids.is_empty()
            || user_id
                .map(|uid| self.allowed_user_ids.contains(&uid))
                .unwrap_or(false);

        let chat_ok = self.allowed_chat_ids.is_empty() || self.allowed_chat_ids.contains(&chat_id);

        user_ok && chat_ok
    }

    pub fn is_path_allowed(&self, path: &Path, base_dir: &Path) -> bool {
        if self.allowed_roots.is_empty() {
            return true;
        }

        let normalized = absolutize(path, base_dir);
        self.allowed_roots
            .iter()
            .any(|root| normalized.starts_with(root))
    }

    pub fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }
}

fn load_allowed_folders_file(path: &Path, base_dir: &Path) -> Result<Vec<PathBuf>> {
    let file_path = absolutize(path, base_dir);
    let raw = fs::read_to_string(&file_path).with_context(|| {
        format!(
            "failed to read whitelist folder file {}",
            file_path.display()
        )
    })?;

    let mut folders = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        folders.push(absolutize(Path::new(trimmed), base_dir));
    }

    Ok(folders)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for path in paths {
        if seen.insert(path.clone()) {
            output.push(path);
        }
    }

    output
}

fn absolutize(path: &Path, base_dir: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };

    normalize_path(&absolute)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                parts.push(component);
            }
        }
    }

    let mut normalized = PathBuf::new();
    for component in parts {
        normalized.push(component.as_os_str());
    }

    normalized
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn user_and_chat_whitelist() {
        let cfg = WhitelistConfig {
            allowed_user_ids: vec![10],
            allowed_chat_ids: vec![20],
            ..WhitelistConfig::default()
        };

        let acl = AccessControl::from_config(&cfg, Path::new("."))
            .expect("access control should initialize");

        assert!(acl.is_user_allowed(Some(10), 20));
        assert!(!acl.is_user_allowed(Some(11), 20));
        assert!(!acl.is_user_allowed(Some(10), 21));
        assert!(!acl.is_user_allowed(None, 20));
    }

    #[test]
    fn folder_whitelist_matches_nested_paths() {
        let cfg = WhitelistConfig {
            allowed_folders: vec![PathBuf::from("/repo/project")],
            ..WhitelistConfig::default()
        };
        let acl = AccessControl::from_config(&cfg, Path::new("."))
            .expect("access control should initialize");

        assert!(acl.is_path_allowed(Path::new("/repo/project/src/main.rs"), Path::new(".")));
        assert!(!acl.is_path_allowed(Path::new("/repo/other/file.rs"), Path::new(".")));
    }

    #[test]
    fn load_folder_whitelist_file() {
        let temp_dir = std::env::temp_dir().join("coding-agent-bot-whitelist-test");
        let _ = fs::create_dir_all(&temp_dir);
        let file_path = temp_dir.join("folders.txt");

        let mut file = fs::File::create(&file_path).expect("temp file should be created");
        writeln!(file, "# comment").expect("write comment");
        writeln!(file).expect("write empty line");
        writeln!(file, "./allowed/a").expect("write path");
        writeln!(file, "./allowed/b").expect("write path");

        let cfg = WhitelistConfig {
            allowed_folders_file: Some(file_path.clone()),
            ..WhitelistConfig::default()
        };
        let acl = AccessControl::from_config(&cfg, &temp_dir)
            .expect("access control should initialize from file");

        assert_eq!(acl.allowed_roots().len(), 2);
    }
}
