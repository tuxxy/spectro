//! Recently opened files, newest first, kept in `<config>/recent`.

use std::path::{Path, PathBuf};

use crate::settings::config_dir;

const MAX: usize = 10;

pub fn load() -> Vec<PathBuf> {
    let Some(text) = config_dir().and_then(|d| std::fs::read_to_string(d.join("recent")).ok()) else {
        return Vec::new();
    };
    text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.is_file()).take(MAX).collect()
}

pub fn save(list: &[PathBuf]) {
    let Some(dir) = config_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let text: String = list.iter().filter_map(|p| p.to_str()).map(|p| format!("{p}\n")).collect();
    let _ = std::fs::write(dir.join("recent"), text);
}

/// Moves `path` to the front of `list`.
pub fn add(list: &mut Vec<PathBuf>, path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(MAX);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupes_and_caps() {
        let mut list: Vec<PathBuf> = (0..12).map(|i| PathBuf::from(format!("/nope/{i}"))).collect();
        add(&mut list, Path::new("/nope/5"));
        assert_eq!(list[0], PathBuf::from("/nope/5"));
        assert_eq!(list.len(), MAX);
        assert_eq!(list.iter().filter(|p| *p == Path::new("/nope/5")).count(), 1);
    }
}
