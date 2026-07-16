use std::{
    cell::Cell,
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::{
    error::{Result, XferError},
    protocol::{EntryKind, TransferKind},
};

#[derive(Clone, Debug)]
pub struct PlannedEntry {
    pub source: PathBuf,
    pub relative: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct TransferPlan {
    pub root_name: String,
    pub kind: TransferKind,
    pub entries: Vec<PlannedEntry>,
    pub total_bytes: u64,
    pub file_count: u64,
    pub skipped_count: u64,
}

pub fn build_plan(input: &Path, excludes: &[String], follow_links: bool) -> Result<TransferPlan> {
    let metadata = fs::symlink_metadata(input).map_err(|error| {
        XferError::invalid_input(format!("cannot inspect {}: {error}", input.display()))
    })?;
    let root_name = input
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            XferError::invalid_input(format!(
                "{} does not have a transferable file name",
                input.display()
            ))
        })?
        .to_string();
    validate_portable_component(&root_name)?;

    if metadata.is_file() {
        return Ok(TransferPlan {
            root_name: root_name.clone(),
            kind: TransferKind::File,
            entries: vec![PlannedEntry {
                source: input.to_path_buf(),
                relative: PathBuf::from(root_name),
                kind: EntryKind::File,
                size: metadata.len(),
            }],
            total_bytes: metadata.len(),
            file_count: 1,
            skipped_count: 0,
        });
    }
    if !metadata.is_dir() {
        return Err(XferError::invalid_input(format!(
            "{} is not a regular file or directory",
            input.display()
        )));
    }

    let matcher = build_excludes(excludes)?;
    let canonical_root = fs::canonicalize(input)?;
    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    let mut file_count = 0_u64;
    let mut skipped_count = 0_u64;
    let excluded_count = Cell::new(0_u64);
    let mut portable_paths = HashSet::new();

    let walker = WalkDir::new(input)
        .follow_links(follow_links)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let Ok(relative) = entry.path().strip_prefix(input) else {
                return true;
            };
            if matcher.is_match(relative) {
                excluded_count.set(excluded_count.get() + 1);
                false
            } else {
                true
            }
        });

    for result in walker.skip(1) {
        let entry = result.map_err(|error| {
            XferError::invalid_input(format!("could not walk {}: {error}", input.display()))
        })?;
        let relative = entry.path().strip_prefix(input).map_err(|_| {
            XferError::invalid_input(format!(
                "{} escaped the transfer root",
                entry.path().display()
            ))
        })?;
        let portable_key = portable_path_key(relative)?;
        if !portable_paths.insert(portable_key) {
            return Err(XferError::invalid_input(format!(
                "{} collides with another path when compared case-insensitively",
                relative.display()
            )));
        }

        let file_type = entry.file_type();
        if file_type.is_symlink() && !follow_links {
            skipped_count += 1;
            continue;
        }
        if follow_links {
            let canonical = fs::canonicalize(entry.path())?;
            if !canonical.starts_with(&canonical_root) {
                return Err(XferError::invalid_input(format!(
                    "followed link {} points outside the transfer root",
                    entry.path().display()
                )));
            }
        }

        if file_type.is_dir() {
            entries.push(PlannedEntry {
                source: entry.path().to_path_buf(),
                relative: relative.to_path_buf(),
                kind: EntryKind::Directory,
                size: 0,
            });
        } else if file_type.is_file() {
            let size = entry
                .metadata()
                .map_err(|error| {
                    XferError::invalid_input(format!(
                        "could not inspect {}: {error}",
                        entry.path().display()
                    ))
                })?
                .len();
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or_else(|| XferError::invalid_input("transfer size exceeds u64"))?;
            file_count += 1;
            entries.push(PlannedEntry {
                source: entry.path().to_path_buf(),
                relative: relative.to_path_buf(),
                kind: EntryKind::File,
                size,
            });
        } else {
            skipped_count += 1;
        }
    }
    skipped_count += excluded_count.get();

    Ok(TransferPlan {
        root_name,
        kind: TransferKind::Directory,
        entries,
        total_bytes,
        file_count,
        skipped_count,
    })
}

pub fn validate_wire_name(name: &str) -> Result<&str> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(XferError::protocol("invalid transfer root name"));
    }
    let path = Path::new(name);
    if path.components().count() != 1
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(XferError::protocol(
            "transfer root contains path separators",
        ));
    }
    validate_portable_component(name).map_err(|_| {
        XferError::protocol("transfer root is not portable across supported platforms")
    })?;
    Ok(name)
}

pub fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let candidate = Path::new(path);
    if candidate.as_os_str().is_empty() || candidate.is_absolute() {
        return Err(XferError::protocol("entry path must be relative"));
    }
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| XferError::protocol("entry path is not valid UTF-8"))?;
                validate_portable_component(part).map_err(|_| {
                    XferError::protocol(format!(
                        "entry path is not portable across supported platforms: {path}"
                    ))
                })?;
            }
            _ => return Err(XferError::protocol(format!("unsafe entry path: {path}"))),
        }
    }
    Ok(candidate.to_path_buf())
}

pub fn choose_destination(output_root: &Path, root_name: &str, overwrite: bool) -> Result<PathBuf> {
    validate_wire_name(root_name)?;
    fs::create_dir_all(output_root)?;
    let preferred = output_root.join(root_name);
    if overwrite || !preferred.exists() {
        return Ok(preferred);
    }

    let source = Path::new(root_name);
    let stem = source
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(root_name);
    let extension = source.extension().and_then(OsStr::to_str);
    for index in 1_u32..=u32::MAX {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = output_root.join(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(XferError::invalid_input(
        "could not find an available destination name",
    ))
}

pub fn path_to_wire(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    XferError::invalid_input(format!("{} is not valid UTF-8", path.display()))
                })?;
                validate_portable_component(part)?;
                parts.push(part);
            }
            _ => {
                return Err(XferError::invalid_input(format!(
                    "{} is not a safe relative path",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

pub(crate) fn portable_path_key(path: &Path) -> Result<String> {
    Ok(path_to_wire(path)?.to_lowercase())
}

fn build_excludes(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| {
            XferError::invalid_input(format!("invalid exclude pattern {pattern:?}: {error}"))
        })?);
        if !pattern.contains('/') {
            builder.add(Glob::new(&format!("**/{pattern}")).map_err(|error| {
                XferError::invalid_input(format!("invalid exclude pattern {pattern:?}: {error}"))
            })?);
        }
    }
    builder
        .build()
        .map_err(|error| XferError::invalid_input(format!("invalid exclude set: {error}")))
}

fn validate_portable_component(component: &str) -> Result<()> {
    if component.is_empty()
        || component.ends_with('.')
        || component.ends_with(' ')
        || component
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return Err(XferError::invalid_input(format!(
            "path component {component:?} is not portable across Windows, macOS, and Linux"
        )));
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if reserved {
        return Err(XferError::invalid_input(format!(
            "path component {component:?} is reserved on Windows"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_relative_path("../secret").is_err());
        assert!(safe_relative_path("/absolute").is_err());
        assert!(safe_relative_path("nested/file.txt").is_ok());
    }

    #[test]
    fn directory_plan_honors_excludes_and_empty_dirs() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("payload");
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("keep.txt"), b"keep").unwrap();
        fs::write(root.join(".git/config"), b"skip").unwrap();

        let plan = build_plan(&root, &[".git".into()], false).unwrap();
        assert_eq!(plan.file_count, 1);
        assert_eq!(plan.total_bytes, 4);
        assert!(
            plan.entries
                .iter()
                .any(|entry| entry.relative == Path::new("empty"))
        );
        assert!(
            plan.entries
                .iter()
                .all(|entry| !entry.relative.starts_with(".git"))
        );
    }

    #[test]
    fn file_plan_reports_size() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("file.bin");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&[1, 2, 3]).unwrap();
        let plan = build_plan(&path, &[], false).unwrap();
        assert_eq!(plan.kind, TransferKind::File);
        assert_eq!(plan.total_bytes, 3);
    }

    #[test]
    fn destination_uses_numbered_name_on_collision() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("photo.jpg"), b"existing").unwrap();
        let destination = choose_destination(directory.path(), "photo.jpg", false).unwrap();
        assert_eq!(destination, directory.path().join("photo (1).jpg"));
    }

    #[test]
    fn rejects_non_portable_and_case_colliding_names() {
        assert!(safe_relative_path("CON.txt").is_err());
        assert!(safe_relative_path("bad:name").is_err());

        assert_eq!(
            portable_path_key(Path::new("Readme")).unwrap(),
            portable_path_key(Path::new("README")).unwrap()
        );
    }
}
