use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Pre-allocate exact file size up-front so workers can seek-write concurrently
/// and there is NO "assembling parts" step at the end.
///
// ponytail: File::set_len (NTFS lazy zero-fill) instead of SetFileValidData —
// good enough; upgrade to win32 sparse/valid-data calls if disk-write stalls show up in profiling.
pub fn preallocate(path: &Path, total: u64) -> io::Result<File> {
    let f = OpenOptions::new().create(true).write(true).open(path)?;
    f.set_len(total.max(1))?;
    Ok(f)
}
