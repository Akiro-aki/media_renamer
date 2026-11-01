//! Moving files to the freedesktop.org trash and back.
//!
//! `--conflict override` sends the file it replaces here, so `mrn undo` can
//! bring it back and the user can still recover it from a file manager.
//! Only trash folders on the file's own file system are used, so trashing is
//! always a rename and never copies a large video across disks:
//! the home trash (`$XDG_DATA_HOME/Trash`) when the file lives on the same
//! file system, otherwise `$topdir/.Trash/$uid` or `$topdir/.Trash-$uid` at
//! the root of the file's mount.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Trash {
    /// The home trash, if a home directory is known.
    home: Option<PathBuf>,
}

impl Trash {
    /// Use `home` as the home trash (tests point it into a temp dir).
    pub fn new(home: Option<PathBuf>) -> Trash {
        Trash { home }
    }

    /// The user's home trash, `$XDG_DATA_HOME/Trash`.
    pub fn from_env() -> Trash {
        let home = directories::BaseDirs::new().map(|b| b.data_dir().join("Trash"));
        Trash { home }
    }

    /// Move `path` into a trash folder on the same file system and return
    /// its new location.
    pub fn put(&self, path: &Path) -> io::Result<PathBuf> {
        let path = std::path::absolute(path)?;
        let dev = fs::symlink_metadata(&path)?.dev();
        let (dir, top) = self.dir_for(&path, dev)?;
        let files = dir.join("files");
        let info = dir.join("info");
        for d in [&files, &info] {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(d)?;
        }

        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("cannot trash a path without a file name"))?;
        let recorded = match top {
            Some(top) => path.strip_prefix(top).unwrap_or(&path),
            None => &path,
        };
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            percent_encode(recorded),
            local_now()
        );

        for n in 1..1000u32 {
            let candidate = if n == 1 {
                name.to_os_string()
            } else {
                let mut s = name.to_os_string();
                s.push(format!(".{n}"));
                s
            };
            let mut info_name = candidate.clone();
            info_name.push(".trashinfo");
            let info_path = info.join(&info_name);
            // Creating the info file first reserves the name (the spec's rule).
            let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            let dest = files.join(&candidate);
            if fs::symlink_metadata(&dest).is_ok() {
                // A stray file without its info entry: leave it and pick another name.
                drop(f);
                let _ = fs::remove_file(&info_path);
                continue;
            }
            let written = f.write_all(body.as_bytes()).and_then(|_| f.sync_all());
            let moved = written.and_then(|_| fs::rename(&path, &dest));
            return match moved {
                Ok(()) => Ok(dest),
                Err(e) => {
                    let _ = fs::remove_file(&info_path);
                    Err(e)
                }
            };
        }
        Err(io::Error::other("no free name in the trash"))
    }

    /// The trash folder for a file on device `dev`, and the mount root its
    /// recorded path is relative to (`None` for the home trash).
    fn dir_for(&self, path: &Path, dev: u64) -> io::Result<(PathBuf, Option<PathBuf>)> {
        if let Some(home) = &self.home
            && nearest_existing(home).is_some_and(|m| m.dev() == dev)
        {
            return Ok((home.clone(), None));
        }

        let top = mount_root(path, dev);
        let uid = fs::metadata("/proc/self")?.uid();

        // An admin-made `.Trash` must be a real directory with the sticky bit.
        let shared = top.join(".Trash");
        if let Ok(m) = fs::symlink_metadata(&shared)
            && m.is_dir()
            && m.mode() & 0o1000 != 0
        {
            let mine = shared.join(uid.to_string());
            let made = fs::DirBuilder::new().mode(0o700).create(&mine);
            if (made.is_ok() || mine.is_dir()) && owned_dir(&mine, uid) {
                return Ok((mine, Some(top)));
            }
        }

        let own = top.join(format!(".Trash-{uid}"));
        let _ = fs::DirBuilder::new().mode(0o700).create(&own);
        if owned_dir(&own, uid) {
            return Ok((own, Some(top)));
        }
        Err(io::Error::other(format!(
            "no usable trash folder on the file system of {}",
            path.display()
        )))
    }
}

/// Move a trashed file back to `to` and drop its info entry.
pub fn restore(trashed: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is occupied", to.display()),
        ));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::ops::move_file(trashed, to)?;
    if let Some(info) = info_path(trashed) {
        let _ = fs::remove_file(info);
    }
    Ok(())
}

/// `<trash>/files/<name>` → `<trash>/info/<name>.trashinfo`
fn info_path(trashed: &Path) -> Option<PathBuf> {
    let name = trashed.file_name()?;
    let dir = trashed.parent()?.parent()?;
    let mut info = name.to_os_string();
    info.push(".trashinfo");
    Some(dir.join("info").join(info))
}

fn nearest_existing(p: &Path) -> Option<fs::Metadata> {
    p.ancestors().find_map(|a| fs::metadata(a).ok())
}

/// The highest ancestor of `path` still on device `dev`.
fn mount_root(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.parent().unwrap_or(path).to_path_buf();
    while let Some(parent) = top.parent() {
        match fs::metadata(parent) {
            Ok(m) if m.dev() == dev => top = parent.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// A real directory (not a symlink) owned by `uid`.
fn owned_dir(p: &Path, uid: u32) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && m.uid() == uid)
}

/// RFC 2396 escaping as the trash spec asks, keeping `/`.
fn percent_encode(p: &Path) -> String {
    let mut out = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Local time as `YYYY-MM-DDThh:mm:ss`, as the trash spec asks.
fn local_now() -> String {
    // SAFETY: `time` with a null pointer only returns the time; `localtime_r`
    // writes into the `tm` we own and is thread-safe.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return "1970-01-01T00:00:00".into();
        }
        tm
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_and_restore() {
        let d = tempfile::tempdir().unwrap();
        let trash = Trash::new(Some(d.path().join("data/Trash")));
        let file = d.path().join("lib/Movie (2020).mkv");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"old").unwrap();

        let t1 = trash.put(&file).unwrap();
        assert!(!file.exists());
        assert_eq!(t1, d.path().join("data/Trash/files/Movie (2020).mkv"));
        let info = fs::read_to_string(info_path(&t1).unwrap()).unwrap();
        assert!(info.contains("Path=/"), "{info}");
        assert!(info.contains("Movie%20%282020%29.mkv"), "{info}");
        assert!(info.contains("DeletionDate="), "{info}");

        // A second file with the same name gets its own slot.
        fs::write(&file, b"older").unwrap();
        let t2 = trash.put(&file).unwrap();
        assert_ne!(t1, t2);

        restore(&t1, &file).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"old");
        assert!(!info_path(&t1).unwrap().exists());
        // Restoring onto an occupied path is refused.
        assert!(restore(&t2, &file).is_err());
        assert!(t2.exists());
    }

    #[test]
    fn encodes_paths() {
        assert_eq!(percent_encode(Path::new("/a b/ü#.mkv")), "/a%20b/%C3%BC%23.mkv");
        let now = local_now();
        assert_eq!(now.len(), 19, "{now}");
        assert_eq!(&now[10..11], "T");
    }
}
