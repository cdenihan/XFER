use std::{
    cell::Cell,
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use unicode_normalization::UnicodeNormalization;
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
    identity: Option<FileIdentity>,
    canonical_source: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    attributes: u32,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
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

fn git_command(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

/// Check remote candidates against the initiating checkout too, so a reverse
/// sync cannot bring back files that are ignored locally but absent on disk.
pub(crate) fn git_ignored_paths<'a>(
    root: &Path,
    paths: impl Iterator<Item = &'a str>,
) -> Result<HashSet<String>> {
    use std::io::{Seek, SeekFrom, Write};
    let root = fs::canonicalize(root)?;
    if !root.ancestors().any(|parent| parent.join(".git").exists()) {
        return Ok(HashSet::new());
    }
    let mut input = tempfile::tempfile()?;
    let mut ignored = HashSet::new();
    for path in paths {
        if Path::new(path)
            .components()
            .any(|part| part.as_os_str() == ".git")
        {
            ignored.insert(path.to_string());
        } else {
            input.write_all(path.as_bytes())?;
            input.write_all(&[0])?;
        }
    }
    input.seek(SeekFrom::Start(0))?;
    let output = git_command(&root)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(input)
        .output()?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(XferError::invalid_input(
            "Git could not check incoming sync paths",
        ));
    }
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        ignored.insert(
            std::str::from_utf8(bytes)
                .map_err(|_| XferError::invalid_input("Git path is not valid UTF-8"))?
                .to_string(),
        );
    }
    Ok(ignored)
}

/// Git supplies its own ignore semantics (nested rules, negations, repository
/// excludes, and tracked-file exceptions). Include parent directories so the
/// existing walker can prune everything else before validation or hashing.
fn git_sync_paths(root: &Path) -> Result<Option<HashSet<PathBuf>>> {
    if !root.ancestors().any(|parent| parent.join(".git").exists()) {
        return Ok(None);
    }
    let output = git_command(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .output()
        .map_err(|error| {
            XferError::invalid_input(format!(
                "cannot read Git ignore rules; Git must be installed: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(XferError::invalid_input(
            "Git could not list files for --gitignore; check repository access",
        ));
    }
    let mut paths = HashSet::new();
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        let name = std::str::from_utf8(bytes)
            .map_err(|_| XferError::invalid_input("Git path is not valid UTF-8"))?;
        let path = Path::new(name);
        if path.components().any(|part| part.as_os_str() == ".git") {
            continue;
        }
        for parent in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
            paths.insert(parent.to_path_buf());
        }
    }
    Ok(Some(paths))
}

pub fn build_plan(input: &Path, excludes: &[String], follow_links: bool) -> Result<TransferPlan> {
    build_plan_with_gitignore(input, excludes, follow_links, false)
}

/// Plan tracked and unignored untracked files when Git filtering is enabled.
pub fn build_plan_with_gitignore(
    input: &Path,
    excludes: &[String],
    follow_links: bool,
    gitignore: bool,
) -> Result<TransferPlan> {
    let metadata = fs::symlink_metadata(input).map_err(|error| {
        XferError::invalid_input(format!("cannot inspect {}: {error}", input.display()))
    })?;
    // Resolve dot/parent directory inputs without changing the names of ordinary
    // inputs (or silently following a root symlink).
    let named_input = if metadata.is_dir() && input.file_name().is_none() {
        fs::canonicalize(input)?
    } else {
        input.to_path_buf()
    };
    let matcher = build_excludes(excludes)?;
    let root_name = named_input
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
                identity: Some(file_identity(&metadata)),
                canonical_source: Some(fs::canonicalize(input)?),
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

    let canonical_root = fs::canonicalize(input)?;
    let git_paths = if gitignore {
        git_sync_paths(&canonical_root)?
    } else {
        None
    };
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
            if matcher.is_match(relative)
                || git_paths
                    .as_ref()
                    .is_some_and(|paths| !paths.contains(relative))
            {
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
        if !file_type.is_dir() && !file_type.is_file() {
            skipped_count += 1;
            continue;
        }

        let portable_key = portable_path_key(relative)?;
        if !portable_paths.insert(portable_key) {
            return Err(XferError::invalid_input(format!(
                "{} collides with another path when compared case-insensitively",
                relative.display()
            )));
        }

        if file_type.is_dir() {
            entries.push(PlannedEntry {
                source: entry.path().to_path_buf(),
                relative: relative.to_path_buf(),
                kind: EntryKind::Directory,
                size: 0,
                identity: None,
                canonical_source: None,
            });
        } else if file_type.is_file() {
            let metadata = entry.metadata().map_err(|error| {
                XferError::invalid_input(format!(
                    "could not inspect {}: {error}",
                    entry.path().display()
                ))
            })?;
            let size = metadata.len();
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or_else(|| XferError::invalid_input("transfer size exceeds u64"))?;
            file_count += 1;
            entries.push(PlannedEntry {
                source: entry.path().to_path_buf(),
                relative: relative.to_path_buf(),
                kind: EntryKind::File,
                size,
                identity: Some(file_identity(&metadata)),
                canonical_source: Some(fs::canonicalize(entry.path())?),
            });
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
    // Path::components normalizes interior dots and repeated separators. Reject
    // those aliases before platform-specific path parsing.
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(XferError::protocol(format!("unsafe entry path: {path}")));
    }
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

pub(crate) fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn choose_destination(output_root: &Path, root_name: &str, overwrite: bool) -> Result<PathBuf> {
    validate_wire_name(root_name)?;
    fs::create_dir_all(output_root)?;
    let preferred = output_root.join(root_name);
    if overwrite || !path_exists(&preferred)? {
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
        if !path_exists(&candidate)? {
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
    Ok(path_to_wire(path)?
        .nfd()
        .flat_map(char::to_lowercase)
        .nfd()
        .collect())
}

pub(crate) fn open_planned_file(entry: &PlannedEntry, follow_links: bool) -> Result<fs::File> {
    let planned_canonical = entry
        .canonical_source
        .as_ref()
        .ok_or_else(|| XferError::security("planned file is missing its canonical path"))?;
    if fs::canonicalize(&entry.source)? != *planned_canonical {
        return Err(XferError::security(format!(
            "{} no longer resolves to its planned location",
            entry.source.display()
        )));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options, follow_links);
    let file = options.open(&entry.source).map_err(|error| {
        XferError::security(format!(
            "could not safely open planned file {}: {error}",
            entry.source.display()
        ))
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() != entry.size
        || entry.identity.as_ref() != Some(&file_identity(&metadata))
        || fs::canonicalize(&entry.source)? != *planned_canonical
    {
        return Err(XferError::security(format!(
            "{} changed after the transfer plan was created",
            entry.source.display()
        )));
    }
    Ok(file)
}

#[cfg(unix)]
fn configure_no_follow(options: &mut fs::OpenOptions, follow_links: bool) {
    use std::os::unix::fs::OpenOptionsExt;

    if !follow_links {
        options.custom_flags(libc::O_NOFOLLOW);
    }
}

#[cfg(windows)]
fn configure_no_follow(options: &mut fs::OpenOptions, follow_links: bool) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    if !follow_links {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

#[cfg(not(any(unix, windows)))]
fn configure_no_follow(_options: &mut fs::OpenOptions, _follow_links: bool) {}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;

    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::windows::fs::MetadataExt;

    FileIdentity {
        attributes: metadata.file_attributes(),
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
    }
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {}
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
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
        || stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
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
        assert!(safe_relative_path("COM¹.txt").is_err());
        assert!(safe_relative_path("lpt²").is_err());
        assert!(safe_relative_path("bad:name").is_err());

        assert_eq!(
            portable_path_key(Path::new("Readme")).unwrap(),
            portable_path_key(Path::new("README")).unwrap()
        );
        assert_eq!(
            portable_path_key(Path::new("\u{e9}.txt")).unwrap(),
            portable_path_key(Path::new("e\u{301}.txt")).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn planned_file_open_rejects_symlink_replacement() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let outside = directory.path().join("outside.txt");
        fs::write(&source, b"planned").unwrap();
        fs::write(&outside, b"outside").unwrap();
        let plan = build_plan(&source, &[], false).unwrap();
        fs::remove_file(&source).unwrap();
        symlink(&outside, &source).unwrap();

        assert!(open_planned_file(&plan.entries[0], false).is_err());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn skipped_symlinks_do_not_create_portable_name_collisions() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("README"), b"included").unwrap();
        symlink("README", root.join("readme")).unwrap();

        let plan = build_plan(&root, &[], false).unwrap();
        assert_eq!(plan.file_count, 1);
        assert_eq!(plan.skipped_count, 1);
    }

    #[test]
    fn empty_directory_plan_has_no_files_or_bytes() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("empty");
        fs::create_dir(&root).unwrap();
        let plan = build_plan(&root, &[], false).unwrap();
        assert_eq!(plan.kind, TransferKind::Directory);
        assert_eq!(plan.file_count, 0);
        assert_eq!(plan.total_bytes, 0);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn invalid_exclude_glob_is_rejected() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("payload");
        fs::create_dir(&root).unwrap();
        assert!(build_plan(&root, &["[".into()], false).is_err());
    }

    #[test]
    fn wire_names_reject_separators_and_special_components() {
        assert!(validate_wire_name("").is_err());
        assert!(validate_wire_name(".").is_err());
        assert!(validate_wire_name("..").is_err());
        assert!(validate_wire_name("nested/file").is_err());
        assert!(validate_wire_name("trailing.").is_err());
        assert_eq!(validate_wire_name("archive.tar").unwrap(), "archive.tar");
    }

    #[test]
    fn destination_numbering_preserves_multi_dot_extension() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("archive.tar.gz"), b"existing").unwrap();
        let destination = choose_destination(directory.path(), "archive.tar.gz", false).unwrap();
        assert_eq!(destination, directory.path().join("archive.tar (1).gz"));
    }

    #[cfg(unix)]
    #[test]
    fn followed_symlink_must_remain_inside_transfer_root() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let outside = directory.path().join("outside.txt");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        assert!(build_plan(&root, &[], true).is_err());
    }
    #[test]
    fn wire_paths_reject_normalized_aliases() {
        for path in ["a/./b", "a//b", "a/", "./a", "a/../b"] {
            assert!(safe_relative_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn file_plan_rejects_invalid_excludes() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("file");
        fs::write(&source, b"hello").unwrap();
        assert!(build_plan(&source, &["[".into()], false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn destination_numbering_skips_dangling_symlinks() {
        let directory = tempdir().unwrap();
        std::os::unix::fs::symlink("missing", directory.path().join("file")).unwrap();
        std::os::unix::fs::symlink("missing", directory.path().join("file (1)")).unwrap();
        assert_eq!(
            choose_destination(directory.path(), "file", false).unwrap(),
            directory.path().join("file (2)")
        );
    }
}

#[cfg(test)]
mod gitignore_tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = git_command(root).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn gitignore_honors_nested_rules_negations_tracked_files_and_manual_excludes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--quiet"]);
        fs::create_dir(root.join("nested")).unwrap();
        fs::create_dir(root.join("build")).unwrap();
        fs::write(
            root.join(".gitignore"),
            "*.log\n!keep.log\n/root-only\nbuild/\n",
        )
        .unwrap();
        fs::write(root.join("nested/.gitignore"), "*.tmp\n!keep.tmp\n").unwrap();
        fs::write(root.join(".git/info/exclude"), "private.txt\n").unwrap();
        for name in [
            "skip.log",
            "keep.log",
            "tracked.log",
            "root-only",
            "nested/root-only",
            "nested/skip.tmp",
            "nested/keep.tmp",
            "nested/skip.log",
            "build/output",
            "private.txt",
            "AccountManager.java",
        ] {
            fs::write(root.join(name), b"data").unwrap();
        }
        git(root, &["add", "--force", "tracked.log"]);
        let plan = build_plan_with_gitignore(root, &[], false, true).unwrap();
        let names = plan
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .map(|entry| path_to_wire(&entry.relative).unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(
            names,
            [
                ".gitignore",
                "nested/.gitignore",
                "keep.log",
                "tracked.log",
                "nested/root-only",
                "nested/keep.tmp",
                "AccountManager.java"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        );
        assert!(
            !plan
                .entries
                .iter()
                .any(|entry| entry.relative.starts_with(".git")
                    || entry.relative.starts_with("build"))
        );
        let subdir = build_plan_with_gitignore(&root.join("nested"), &[], false, true).unwrap();
        assert!(
            !subdir
                .entries
                .iter()
                .any(|entry| entry.relative == Path::new("skip.log"))
        );
        assert!(
            subdir
                .entries
                .iter()
                .any(|entry| entry.relative == Path::new("keep.tmp"))
        );
        let excluded =
            build_plan_with_gitignore(root, &["tracked.log".into()], false, true).unwrap();
        assert!(
            !excluded
                .entries
                .iter()
                .any(|entry| entry.relative == Path::new("tracked.log"))
        );
        let unfiltered = build_plan(root, &[], false).unwrap();
        assert!(
            unfiltered
                .entries
                .iter()
                .any(|entry| entry.relative == Path::new("skip.log"))
        );
    }

    #[test]
    fn gitignore_is_inert_outside_a_repository() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(".gitignore"), "*.log").unwrap();
        fs::write(directory.path().join("keep.log"), b"data").unwrap();
        let plan = build_plan_with_gitignore(directory.path(), &[], false, true).unwrap();
        assert_eq!(plan.file_count, 2);
    }

    #[test]
    fn gitignore_supports_git_file_markers_and_paths_with_spaces() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout");
        fs::create_dir(&root).unwrap();
        git(
            &root,
            &["init", "--quiet", "--separate-git-dir", "../metadata"],
        );
        assert!(root.join(".git").is_file());
        fs::write(root.join(".gitignore"), "*.class\n").unwrap();
        fs::write(root.join("Account Manager.java"), b"source").unwrap();
        fs::write(root.join("Account Manager.class"), b"compiled").unwrap();
        let plan = build_plan_with_gitignore(&root, &[], false, true).unwrap();
        assert_eq!(plan.file_count, 2);
        assert!(
            plan.entries
                .iter()
                .any(|entry| entry.relative == Path::new("Account Manager.java"))
        );
    }
}
