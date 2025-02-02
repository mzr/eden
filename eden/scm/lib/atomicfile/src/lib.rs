/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{fs, fs::File, io, path::Path};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt};

/// Create a temp file and then rename it into the specified path to
/// achieve atomicity. The temp file is created in the same directory
/// as path to ensure the rename is not cross filesystem. If fysnc is
/// true, the file will be fsynced before and after renaming, and the
/// directory will by fsynced after renaming.
///
/// mode_perms is required but does nothing on windows. mode_perms is
/// not automatically umasked.
///
/// The renamed file is returned. Any further data written to the file
/// will not be atomic since the file is already visibile to readers.
///
/// Note that the rename operation will fail on windows if the
/// destination file exists and is open.
pub fn atomic_write<P: AsRef<Path>>(
    path: P,
    #[allow(dead_code)] mode_perms: u32,
    fsync: bool,
    op: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<File> {
    let dir = match path.as_ref().parent() {
        Some(dir) => dir,
        None => return Err(io::ErrorKind::InvalidInput.into()),
    };

    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    let f = temp.as_file_mut();

    #[cfg(unix)]
    f.set_permissions(Permissions::from_mode(mode_perms))?;

    op(f)?;

    if fsync {
        f.sync_data()?;
    }

    let max_retries = if cfg!(windows) { 5u16 } else { 0 };
    let mut retry = 0;
    loop {
        match temp.persist(&path) {
            Ok(persisted) => {
                if fsync {
                    persisted.sync_all()?;

                    // Also sync the directory on Unix.
                    // Windows does not support syncing a directory.
                    #[cfg(unix)]
                    {
                        if let Ok(opened) = fs::OpenOptions::new().read(true).open(dir) {
                            let _ = opened.sync_all();
                        }
                    }
                }

                break Ok(persisted);
            }
            Err(e) => {
                if retry == max_retries || e.error.kind() != io::ErrorKind::PermissionDenied {
                    break Err(e.error);
                }

                // Windows fails with "Access Denied" if destination file is open.
                // Retry a few times.
                tracing::info!(
                    name = "atomic_write rename failed with EPERM. Will retry.",
                    retry = retry,
                    path = AsRef::<str>::as_ref(&path.as_ref().display().to_string()),
                );
                std::thread::sleep(std::time::Duration::from_millis(1 << retry));
                temp = e.file;

                retry += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::prelude::MetadataExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_atomic_write() -> io::Result<()> {
        let td = tempdir()?;

        let foo_path = td.path().join("foo");
        atomic_write(&foo_path, 0o640, false, |f| {
            f.write_all(b"sushi")?;
            Ok(())
        })?;

        // Sanity check that we wrote contents and the temp file is gone.
        assert_eq!("sushi", std::fs::read_to_string(&foo_path)?);
        assert_eq!(1, std::fs::read_dir(td.path())?.count());

        // Make sure we can set the mode perms on unix.
        #[cfg(unix)]
        assert_eq!(
            0o640,
            0o777 & std::fs::File::open(&foo_path)?.metadata()?.mode()
        );

        Ok(())
    }
}
