use std::path::{Path, PathBuf};

/// Try the longest recorded path suffix under the chosen source directory first.
pub fn resolve(root: &str, recorded: &str) -> PathBuf {
    let root = root.trim();
    let normalized = recorded.replace('\\', "/");
    if !root.is_empty() {
        let parts: Vec<_> = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        for start in 0..parts.len() {
            let suffix = &parts[start..];
            if suffix
                .iter()
                .any(|part| *part == "." || *part == ".." || part.contains(':'))
            {
                continue;
            }
            let candidate = suffix
                .iter()
                .fold(PathBuf::from(root), |path, part| path.join(part));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    Path::new(recorded).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_longest_suffix_and_windows_paths_without_traversal() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/main.c"), "").unwrap();
        std::fs::write(root.path().join("main.c"), "").unwrap();
        let root = root.path().to_str().unwrap();
        assert_eq!(
            resolve(root, "C:\\build\\src\\main.c"),
            Path::new(root).join("src/main.c")
        );
        assert_eq!(
            resolve(root, "/missing/file.c"),
            PathBuf::from("/missing/file.c")
        );
        assert_eq!(resolve("", "/old/main.c"), PathBuf::from("/old/main.c"));
    }
}
