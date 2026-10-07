use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn directory(path: &Path) -> Result<()> {
    // Check every existing ancestor before creating a child. Reparse directories
    // are excluded even if they are not represented as symlinks by std.
    for p in path.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(m) => {
                ensure!(!m.file_type().is_symlink(), "output path contains a link");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        m.file_attributes() & 0x400 == 0,
                        "output path contains a reparse point"
                    );
                }
                ensure!(m.is_dir(), "output ancestor is not a directory");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    fs::create_dir_all(path).context("cannot create export directory")?;
    Ok(())
}

pub fn write_archive(dir: &Path, doc: &Value, uid: u64, timestamp: u64) -> Result<PathBuf> {
    directory(dir)?;
    let stem = format!("lilypad-export-{uid}-{timestamp}-{}", std::process::id());
    let dest = dir.join(format!("{stem}.json"));
    let temp = dir.join(format!("{stem}.part"));
    ensure!(!dest.try_exists()?, "export already exists");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        serde_json::to_writer_pretty(&mut file, doc)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        let reread: Value = serde_json::from_reader(std::fs::File::open(&temp)?)?;
        ensure!(reread == *doc, "export readback verification failed");
        commit(&temp, &dest).context("cannot commit export file")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(dest)
}

fn commit(temp: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = dest.as_os_str().encode_wide().chain(Some(0)).collect();
        // Deliberately omit REPLACE_EXISTING: a file created between the first
        // existence probe and commit must survive. std::fs::rename replaces it.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(temp, dest)?;
        fs::remove_file(temp)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomically_commits_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("hbr-export-test-{}", std::process::id()));
        let doc = serde_json::json!({"format":"lilypad-export/1","tables":{"user_item":[]}});
        let path = write_archive(&dir, &doc, 1, 10).unwrap();
        assert!(write_archive(&dir, &doc, 1, 10).is_err());
        assert_eq!(
            serde_json::from_reader::<_, Value>(fs::File::open(&path).unwrap()).unwrap(),
            doc
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn final_commit_preserves_a_destination_created_after_the_initial_probe() {
        let dir = std::env::temp_dir().join(format!("hbr-export-race-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let temp = dir.join("new.part");
        let dest = dir.join("existing.json");
        fs::write(&temp, "new export").unwrap();
        fs::write(&dest, "original export").unwrap();
        assert!(commit(&temp, &dest).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "original export");
        fs::remove_file(temp).unwrap();
        fs::remove_file(dest).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
