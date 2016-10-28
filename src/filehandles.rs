/// Handles to opened inodes. Mainly indicates that they should not be garbage collected by the inode cache.

use std::collections::HashMap;
use std::sync::Mutex;
use std::fs::File;

pub type FileHandle = u64;
pub type Inode = u64;

pub struct HotFiles {
    mutex : Mutex<HotFilesMutexed>
}

struct FileEntry {
    file : Option<File>,
    ino : Inode,
}

struct HotFilesMutexed {
    by_fh : HashMap<FileHandle, FileEntry>,
    by_ino : HashMap<Inode, u64>, // a count for handled files because of the hard link case.
    count : u64,
}

impl HotFiles {
    pub fn new() -> HotFiles {
        debug!("Creating a hot file cache!");
        HotFiles {
            // TODO Use other hasher.
            mutex : Mutex::new(
                HotFilesMutexed {
                    by_fh : HashMap::new(),
                    by_ino : HashMap::new(),
                    count : 0,
                }
            )
        }
    }
    pub fn make_handle(&mut self, file: Option<File>, ino: Inode) -> FileHandle {
        let mut hot = self.mutex.lock().expect("This is not supposed to happen...");
        let count = hot.count + 1;
        hot.by_fh.insert(count, FileEntry{file: file, ino: ino});
        *hot.by_ino.entry(ino).or_insert(0) += 1 ;
        trace!("Got handle {} for inode {}", count, ino);
        hot.count = count;
        count
    }
    pub fn take_file(&mut self, fh: FileHandle) -> File {
        let mut hot = self.mutex.lock().expect("This is not supposed to happen...");
        hot.by_fh.remove(&fh).unwrap().file.unwrap()//directory functions should not take any handle.
    }
    pub fn restore_file(&mut self, fh: FileHandle, file: File, ino: Inode) {
        let mut hot = self.mutex.lock().expect("This is not supposed to happen...");
        hot.by_fh.insert(fh, FileEntry{file: Some(file), ino: ino});
    }
    pub fn release_handle(&mut self, fh: FileHandle) {
        let mut hot = self.mutex.lock().expect("This is not supposed to happen...");
        let ino = match hot.by_fh.remove(&fh) {
            Some(entry) => entry.ino,
            None => {
                // TODO: improve this.
                error!("This is badly handled: file (handle={}) was taken but not yet restored, and now the attempt to release fails temporarily.", fh);
                return;
            }
        };
        match hot.by_ino.remove(&ino).unwrap() {
            mut count if count > 1 => {
                count -= 1;
                trace!("Decreasing count to {} for inode {}", count, ino);
                hot.by_ino.insert(ino, count);
            },
            _ => {
                trace!("Inode {} is not hot any longer.", ino);
            }
        }
    }
    pub fn is_hot(&self, ino: Inode) -> bool {
        let hot = self.mutex.lock().expect("This is not supposed to happen...");
        hot.by_ino.contains_key(&ino)
    }
}
