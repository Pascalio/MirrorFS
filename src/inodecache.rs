/// lightweight universal inode cache.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::path;
use std::time;
use std::cmp::Ordering;

use filehandles::{Inode, HotFiles};
// TODO : release_handle frees cache. cf gc index's todo.

const MARGIN: usize = 100;// to avoid too much reallocation.
const PAD: usize = 10; // autoremove starts when only PAD entries are free.
const MIN_USABLE: usize = 20; // autoremove stops before MIN_USABLE entries get freed.
const MIN_AGE : u64 = 1; // Do not remove entries younger than MIN_AGE seconds.

/// A ring-buffer-ish index value, which wraps over to 0 after reaching parametrical max.
#[derive(Clone, Copy)]
struct Index {
    current : usize,
    max : usize,
    initial_max : usize,
}
// TODO : gc index must keep max to extended value till margin is freed! Possibility of a some memory leak on MARGIN if an inode was hot when gc passed and the margin is not used any longer : links in the map remain stored undefinitely. This is not a tremedous issue though.
impl Index {
    fn new(max: usize) -> Index {
        Index {
            current : 0,
            max : max,
            initial_max : max,
        }
    }
    fn inc(&mut self) {
        if self.current != self.max {
            self.current += 1;
        } else {
            self.current = 0;
            self.max = self.initial_max;
        }
    } // in case of saturation of the ring journal.
    fn extend(&mut self, additional: usize) {
        self.max += additional;
    }
    fn nb(&self) -> usize {
        self.current
    }
    fn is_back_close(&self, other: &Index) -> bool {
        if self.current > other.current {
            (self.max - self.current)
            + other.current
            <= PAD
        } else {
            other.current - self.current
            <= PAD
        }
    }
}

impl PartialEq for Index {
     fn eq(&self, other: &Index) -> bool {
         self.current == other.current
     }
}
impl PartialOrd for Index {
    fn partial_cmp(&self, other: &Index) -> Option<Ordering> {
        // The design accepts the case where Index starts over from 0 and is evaluated as smaller the other, although it actually is more advanced.
        self.current.partial_cmp(&other.current)
    }
}

struct InoMapValue {
    index : usize,/*journal index*/
    links : HashSet<path::PathBuf>,
}

type InoMap = HashMap<Inode, InoMapValue>;

type Journal = Vec<JournalEntry>;

trait Journaling {
    fn create(max: usize) -> Journal;
    fn grow(&mut self, bottom: usize);
}
#[derive(Clone)]
struct JournalEntry {
    ino : Inode,
    time : time::Instant,
}
impl JournalEntry {
    fn empty() -> JournalEntry {
        JournalEntry {
            ino : 0,
            time : time::Instant::now(), // TODO : optimize.
        }
    }
}

impl Journaling for Vec<JournalEntry> {
    fn create(max: usize) -> Journal {
        let mut j = Journal::with_capacity(max + MARGIN);
        j.resize(max, JournalEntry::empty());
        j
    }
    // Partially reallocate.
    fn grow(&mut self, bottom: usize) {
        // This may fully reallocate the Vec somewhere else, but hopefully (and generally) only partially reallocates items inside the MARGINed capacity of the Vec.
        debug!("Journal has to grow!");
        let top = self.len();
        self.resize(top + MARGIN, JournalEntry::empty());
        for i in (bottom..top).rev() {
            trace!("Reallocating 1 journal element at address {}...", i + MARGIN);
            let ino = self.remove(i);
            self.insert(i + MARGIN, ino);
        }
        trace!("Journal grew!");
    }
}

struct InodeCacheMutex {
    map : InoMap,
    journal : Journal,
    position : Index,// position of lastly stored element, not of available slot.
    gc_index : Index,
    min_age : time::Duration,
}
impl InodeCacheMutex {
    pub fn journal_recycle(&mut self, index: usize, hot: &HotFiles) -> bool {
        let ino = self.journal[index].ino;
        if hot.is_hot(ino) {
            if self.map.get(&ino).unwrap().index == index {
                trace!("Inode {} is hot (in use), so unfit for recycling. map index = {}, journal index = {}", ino, self.map.get(&ino).unwrap().index, index);
                false
            } else {
                trace!("Inode {} is hot but already referenced.", ino);
                self.journal[index].ino = 0;
                trace!("Deassociated inode {} from journal index {}", ino, index);
                true
            }
        } else {
            trace!("Inode {} is cold (not in use), so fit for recycling.", ino);
            self.journal[index].ino = 0;
            trace!("Deassociated inode {} from journal index {}", ino, index);
            true
        }
    }
    pub fn autoremove(&mut self, hot: &HotFiles) -> usize {
        let mut acc = 0;
        if self.position.is_back_close(&self.gc_index) {
            for _ in 0..MIN_USABLE { // TODO: implement with a while MIN_USABLE and age checks.
                if self.journal[self.gc_index.nb()].time.elapsed() <= self.min_age {
                    trace!("Cache entries are still too new to be freed.");
                    break;
                }
                let ino = self.journal[self.gc_index.nb()].ino;
                if hot.is_hot(ino) {
                    self.gc_index.inc();
                } else {
                    match self.map.remove(&ino) {
                        Some(entry) => {
                            if entry.index != self.gc_index.nb() {
                                trace!("entry.index = {} for inode {} but gc_index = {}",entry.index, ino,  self.gc_index.nb()); //TOREMOVE
                                self.journal[self.gc_index.nb()].ino = 0;
                                trace!("Removed unused entry from journal.");
                                self.map.insert(ino, entry);
                                self.gc_index.inc();
                            } else {
                                debug!("Removed all inode associations for {} from cache", ino);
                                for p in entry.links.iter() {
                                    trace!("- {}",p.display());
                                    acc += p.as_os_str().len() * 2/*size of unicode*/;
                                }
                                self.gc_index.inc();
                            }
                        },
                        None => {
                            self.journal[self.gc_index.nb()].ino = 0;
                            trace!("Removed unused entry from journal.");
                            self.gc_index.inc();
                        }
                    }
                }
            }
        }
        trace!("Autoremoved {} bytes from inode cache", acc);
        acc
    }
}

pub struct InodeCache {
    // Fuse mostly runs in multithreaded mode, right ?
    inode_mutex : Mutex<InodeCacheMutex>,
    // Independent mutex for HotFiles.
    pub hot_files : HotFiles,
    // approximate
    total_size : usize,
}
impl InodeCache {
    pub fn new (size: usize, min_age: u64) -> InodeCache {
        info!("Creating a new Inode Cache.");
        let size = if size > (PAD + MIN_USABLE) {size} else {
            info!("Desired cache size is too small: falling back to minimum usable + pad = {}", PAD + MIN_USABLE);
            PAD + MIN_USABLE
        };
        let min_age = if min_age > MIN_AGE {min_age} else {
            info!("Desired cache age is too little: falling back to {}", MIN_AGE);
            MIN_AGE
        };
        InodeCache {
            inode_mutex :
                Mutex::new(InodeCacheMutex{
                    //TODO other hasher
                    map : InoMap::with_capacity(size + MARGIN),
                    journal : Journal::create(size),
                    position : Index::new(size - 1), // size = 1 && index = 0.
                    gc_index : Index::new(size - 1),
                    min_age : time::Duration::from_secs(min_age),
                }
            ),
            hot_files : HotFiles::new(),
            total_size : (size + MARGIN) * 2 // JournalEntries
                        + (size + MARGIN) * 2 // roughly InoMapValues
                        + (size + MARGIN) * 2, // very roughly Journal + InoMap
        }
    }
    pub fn store(&mut self, ino : Inode, path : &path::Path, pid : u32) {
        let mut i = self.inode_mutex.lock().expect("This is not supposed to happen...");
        let owned_path = path.to_path_buf();
        let start_index = i.position;
        loop {// This loop hopefully never executes more than once!
            i.position.inc();
            while start_index != i.position /*break when cycled around*/ {
                let index = i.position.nb();
                // Find empty space in Journal ring.
                if i.journal[index].ino == 0
                ||
                i.journal_recycle(index, &self.hot_files) {
                    // Store the reference in the Journal
                    i.journal[index].ino = ino;
                    i.journal[index].time = time::Instant::now();
                    // And store the index and resolution by Inode in the InoMap.
                    {
                        let mut entry = i.map.entry(ino).or_insert(
                            InoMapValue{
                                index : 0,
                                links : HashSet::new(),
                            }
                        );
                        entry.index = index;
                        let len = owned_path.as_os_str().len();
                        if entry.links.insert(owned_path) {
                            self.total_size += len * 2/*size of unicode*/;
                        }
                        trace!("Associated inode {} to journal index {}", ino, index);
                    }
                    self.total_size -= i.autoremove(&self.hot_files);
                    return;
                } else {
                    i.position.inc();
                }
            }
            let index = i.position.nb();
            i.journal.grow(index + 1); // index is to keep in place, the next ( +1) is to be reallocated.
            i.position.extend(MARGIN);
            i.gc_index.extend(MARGIN);
        }
    }
    pub fn resolve(&self, ino: Inode) -> path::PathBuf {
        let i = self.inode_mutex.lock().expect("This is not supposed to happen...");
        if let Some(entry) = i.map.get(&ino) {
            trace!("Learning from the cache : path {:?} for inode {}", entry.links.iter().next().unwrap().display(), ino);
            entry.links.iter().next().unwrap().clone()
        } else {
            // This is to be improved...
            error!("This is not supposed to happen... Inode {} could not be found in the inode cache!\nDoes your application implement an internal inode cache ??", ino);
            path::PathBuf::from("")
        }
    }
    pub fn remove(&mut self, ino: Inode, link: Option<&path::Path>, pid: u32, count: usize) {
        let mut i = self.inode_mutex.lock().expect("This is not supposed to happen...");
        let mut acc = 0;
        p!(i.map.len());
        if !i.map.contains_key(&ino) {
            // This is not so uncommon because certain process call this after an unsuccessful call to lookup, or after having unlinked the file (which automatically shrinks the cache). Downgrade to warn!() ?
            error!("Tried to remove from cache an inode entry that did not exist... Inode = {}, Process = {}", ino, pid);
            return;
        }
        let mut entry = i.map.remove(&ino).unwrap();
        if link.is_none() {
            trace!("Removing the whole inode map for inode {}, as well as its journal entry", ino);
            i.journal[entry.index].ino = 0;
            for p in entry.links.iter() {
                acc += p.as_os_str().len() * 2/*size of unicode*/;
            }
        } else {
            trace!("Removing link \"{}\" from cache for inode {}", link.unwrap().display(), ino);
            entry.links.remove(link.unwrap());
            if entry.links.is_empty() {
                trace!("No more link associated to inode {}, removing entry from cache.", ino);
                i.journal[entry.index].ino = 0;
                for p in entry.links.iter() {
                    acc += p.as_os_str().len() * 2/*size of unicode*/;
                }

            } else {
                i.map.insert(ino, entry);
                self.total_size -= link.unwrap().as_os_str().len();
            }
        }
        trace!("Freed {} bytes in inode cache.", acc);
        self.total_size -= acc;
        p!(i.map.len());
    }
    pub fn print_stats(&self) {
        trace!("Approximate size of inode cache is of {} bytes ({} usize units)", self.total_size * 4, self.total_size);
        let i = self.inode_mutex.lock().expect("This is not supposed to happen...");
        for (n, e) in i.journal.iter().enumerate() {
            if e.ino != 0 {
                trace!("age of journal entry {} is {:?}", n, e.time.elapsed())
            }
        }
    }
}
