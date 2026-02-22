use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};

pub fn normalize_relative_path(value: &str) -> Result<String> {
    let path = Path::new(value);
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Normal(c) => normalized.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("path escapes root"));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("absolute paths are not allowed"));
            }
        }
    }

    let normalized_str = normalized
        .to_str()
        .ok_or_else(|| anyhow!("path contains invalid unicode"))?;
    Ok(normalized_str.to_string())
}

pub fn resolve_under_root(root: &Path, request_path: &str) -> Result<PathBuf> {
    let normalized = normalize_relative_path(request_path)?;
    Ok(root.join(normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_valid_relative_paths() {
        assert_eq!(
            normalize_relative_path("foo/bar/baz.json").expect("valid path"),
            "foo/bar/baz.json"
        );
    }

    #[test]
    fn removes_dot_segments() {
        assert_eq!(
            normalize_relative_path("./foo/./bar.txt").expect("valid path"),
            "foo/bar.txt"
        );
    }

    #[test]
    fn rejects_parent_escape() {
        assert!(normalize_relative_path("../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_absolute_paths() {
        assert!(normalize_relative_path("/var/data/file").is_err());
    }
}
