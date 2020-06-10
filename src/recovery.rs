//! Recovery utilities for queues left in as inconsistent state. Use these
//! functions if you need to automatically recover from a failure.
//!
//! This module is dependent on the `recovery` feature, which is enabled
//! by default.
//!
use std::fs::*;
use std::io;
use std::path::Path;
use sysinfo::*;

use super::state::{FilePersistence, QueueState};
use super::{recv_lock_filename, send_lock_filename, FileGuard};

/// Unlocks a lock file if the owning process does not exist anymore.
/// # Panics
///
/// This function panics if it cannot parse the lockfile.
fn unlock<P: AsRef<Path>>(lock_filename: P) -> io::Result<()> {
    let contents = read_to_string(&lock_filename)?;
    let owner_pid = contents[4..]
        .parse::<sysinfo::Pid>()
        .expect("failed to parse recv lock file");
    let system = System::new_with_specifics(RefreshKind::new().with_processes());

    if system.get_processes().get(&owner_pid).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "process {} is still locking `{:?}`",
                owner_pid,
                lock_filename.as_ref()
            ),
        ));
    } else {
        remove_file(lock_filename)?;
        Ok(())
    }
}

/// Unlocks a queue in a given directory for sending. This function returns an
/// error of kind `io::ErrorKind::Other` when the process listed in the
/// lockfile still exists.
///
/// # Panics
///
/// This function panics if it cannot parse the lockfile.
pub fn unlock_for_sending<P: AsRef<Path>>(base: P) -> io::Result<()> {
    unlock(send_lock_filename(base.as_ref()))
}

/// Unlocks a queue in a given directory for receiving. This function returns
/// an error of kind `io::ErrorKind::Other` when the process listed in the
/// lockfile still exists.
///
/// # Panics
///
/// This function panics if it cannot parse the lockfile.
pub fn unlock_for_receiving<P: AsRef<Path>>(base: P) -> io::Result<()> {
    unlock(recv_lock_filename(base.as_ref()))
}

/// Unlocks a queue in a given directory for both sending and receiving. This
/// function is the combination of [`unlock_for_sending`] and
/// [`unlock_for_receiving`].
///
/// # Panics
///
/// This function panics if it cannot parse either of the lock files.
pub fn unlock_queue<P: AsRef<Path>>(base: P) -> io::Result<()> {
    unlock_for_sending(base.as_ref())?;
    unlock_for_receiving(base.as_ref())?;

    Ok(())
}

/// Guesses the send metadata for a given queue. This equals to the top
/// position in the greatest segment present in the directory. This function
/// will substitute the current send metadata by this guess upon acquiring
/// the send lock on this queue.
///
/// # Panics
///
/// This function panics if
/// 1. there is a file in the queue folder with extension `.q` whose name is
/// not an integer, such as `foo.q`.
pub fn guess_send_metadata<P: AsRef<Path>>(base: P) -> io::Result<()> {
    // Lock for sending:
    let lock = FileGuard::try_lock(send_lock_filename(base.as_ref()))?;

    // Find greatest segment:
    let mut max_segment = 0;
    for maybe_entry in read_dir(base.as_ref())? {
        let path = maybe_entry?.path();
        if path.extension().map(|ext| ext == "q").unwrap_or(false) {
            let segment = path
                .file_stem()
                .expect("has extension, therefore has stem")
                .to_string_lossy()
                .parse::<u64>()
                .expect("failed to parse segment filename");

            max_segment = u64::max(segment, max_segment);
        }
    }

    // Find top position in the segment:
    let segment_metadata = metadata(base.as_ref().join(format!("{}.q", max_segment)))?;
    let position = segment_metadata.len();

    // Generate new queue state:
    let queue_state = QueueState {
        segment: max_segment,
        position,
        ..QueueState::default()
    };
    
    // And save:
    let mut persistence = FilePersistence::new();
    let _ = persistence.open_send(base.as_ref())?;
    persistence.save(&queue_state)?;

    // Drop lock for sending:
    drop(lock);

    Ok(())
}