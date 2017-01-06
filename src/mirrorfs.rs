extern crate time;
extern crate multimap;

use std::{path, fs, io};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use fuse::*;
use time::*;
use libc::{c_int, ENOSYS, ENOENT, EEXIST, O_RDWR, O_RDONLY, O_WRONLY, O_APPEND, O_TRUNC};
use libc;
use self::multimap::MultiMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::mem;
use std::ffi;
use std::slice;
use nix;
use nix::NixPath;
use utime;
use capabilities::*;

use inodecache::*;
use helper::*;
use user::*;
#[cfg(feature="enable_unsecure_features")]
use fasthashes::*;

const WRITE : u32 = 2;
const READ : u32 = 4;
// TODO: fix this.
//const RDEX : u32 = 5; // On directories, read only allows to get names in the inode, and execute allows to get metadata (and hence to do something with the content of the dir, like proceding to subdirectories)
//const RDWR : u32 = 6;

// TODO : What is TTL ?????
const TTL: Timespec = Timespec { sec: 1, nsec: 0 };                 // 1 second

#[cfg(feature="enable_unsecure_features")]
pub struct Settings {
	pub uid : Uid,
	pub gid : Gid,
	pub fullaccess : FastSet<u32>,
	// TODO: This implements process to disk mapping. Should we emplement reverse disk to process mapping too ?
	pub user_map : FastMap<Uid, Uid>,
	pub group_map : FastMap<Gid, Gid>,
	pub caps : Capabilities,
}
#[cfg(not(feature="enable_unsecure_features"))]
pub struct Settings {
	pub uid : Uid,
	pub gid : Gid,
	pub caps : Capabilities,
}
impl Settings {
	pub fn has_cap(&self, cap: Capability) -> bool {
		self.caps.check(cap, Flag::Permitted)
	}
}

pub struct MirrorFS {
    base_path : String,// base path: where FS looks into.
    virtual_path : String,
    // Use another hasher for efficency.
    inodes : InodeCache,
    dentry_cache : MultiMap<u64, Vec<io::Result<fs::DirEntry>>>,
    pub settings : Settings,
}

impl MirrorFS {
	#[cfg(feature="enable_unsecure_features")]
    pub fn new(base_path : &str, virtual_path : &str, uid: Uid, gid : Gid, user_map : FastMap<Uid, Uid>, group_map : FastMap<Gid, Gid>, fullaccess:FastSet<u32>, caps: Capabilities) -> MirrorFS {
        let mut fs = MirrorFS {
            base_path : base_path.to_owned(),
            virtual_path : virtual_path.to_owned(),
            inodes : InodeCache::new(10, 2),
            dentry_cache : MultiMap::new(),
            settings : Settings {
				uid : uid,
				gid : gid,
				fullaccess : fullaccess,
				user_map: user_map,
				group_map: group_map,
				caps : caps,
			},
        };
        fs.inodes.store(1, &path::Path::new(base_path).to_path_buf(), 0);
        fs.inodes.print_stats();
        fs.inodes.hot_files.make_handle(None, 1); // This ensures inode 1 is never removed from cache. (always "hot")
        fs
    }
    #[cfg(not(feature="enable_unsecure_features"))]
    pub fn new(base_path : &str, virtual_path : &str, uid: Uid, gid : Gid, caps: Capabilities) -> MirrorFS {
        let mut fs = MirrorFS {
            base_path : base_path.to_owned(),
            virtual_path : virtual_path.to_owned(),
            inodes : InodeCache::new(10, 2),
            dentry_cache : MultiMap::new(),
            settings : Settings {
				uid : uid,
				gid : gid,
				caps : caps,
			},
        };
        fs.inodes.store(1, &path::Path::new(base_path).to_path_buf(), 0);
        fs.inodes.print_stats();
        fs.inodes.hot_files.make_handle(None, 1); // This ensures inode 1 is never removed from cache. (always "hot")
        fs
    }

    pub fn mount<P: AsRef<path::Path>>(self, mountpoint : &P) {
		// Mount options as if from the command line!
        mount(self, mountpoint, &["-oallow_other".as_ref()]);
    }

    /// take a 0-depth relative path from the virtual directory and return an absolute path to the base directory's element of the mirroring.
    fn name2original (&mut self, name: &path::Path, parent : u64, req: &Request, access_on_parent: u32) -> Result<path::PathBuf, i32> {
        // Get Base path of the mirroring.
        let mut original = path::PathBuf::from(&self.base_path);
        // Find parent path relative to the Base.
        if parent != 1 {
            trace!("Parent is not 1 -> looking for parent in inode cache !");
            let parent_path = self.inodes.resolve(parent);
            // Get absolute path to parent of requested path.
            original.push(parent_path);
        }
        // Absolute requested path.
        trace!("So, lookup of name {} in directory {} translated to {}/{}", name.display(), parent, original.display(), name.display());
        // For forward compatibility: returning a Result will probably have some use later on.
        Ok(original.join(name))
    }
}

/* TODO :
- figure out what fsyncdir() is, how it is to be implemented and whether it is to be implemented.
- getlk ?
- race conditions before ReplyEntry...
- could we avoid copying some pathbufs ?
- deduplicate error messages into constants.
*/
impl Filesystem for MirrorFS {
    fn init(&mut self, _req: &Request) -> Result<(), c_int> {
        info!{"MirrorFS was initialized !"};
        // spawn_mount other FS.
        Ok(())
    }

    fn destroy (&mut self, _req: &Request) {
        info!("MirrorFS was unmounted, and is now about to be destroyed!");
        //unmount other FS.
    }
    // Translate path to inode. Also get file attributes. Generation = ?
    // parent is ino of dir.
    fn lookup (&mut self, _req: &Request, parent: u64, name: &path::Path, reply: ReplyEntry) {
        debug!("lookup of \"{}\" in inode {} for process {} in user account {}...", name.display(), parent, _req.pid(), _req.uid());

        p!(_req.pid()); // when not found, inode is watched by ??? the kernel? which dispatches in user's account process to call lookup and get attributes and read directory.
        p!(_req.uid());

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let path_base = match self.name2original(name, parent, _req, READ) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };
        // symlink_metadata avoids "dereferencing" symlinks : otherwise, metadata() would yield the metadata of the link's target, of course.
        match path_base.symlink_metadata() {
            Ok(md) => {
				self.inodes.store(md.ino(), &path_base, _req.pid());
				let attr : FileAttr = fill_file_attr(&md);
				reply.entry(&TTL, &attr, 0);
			},
             Err(error) => {
                 warn!("Could not lookup {} : {:?}", path_base.display(), error);
                 reply.error(error.raw_os_error().unwrap());
             },
        }
        self.inodes.print_stats();
    }

    fn forget (&mut self, _req: &Request, _ino: u64, _nlookup: u64) {
        debug!("forget callback for ino {} # lookups {}.", _ino, _nlookup);
        self.inodes.remove(_ino, None, _req.pid(), _nlookup as usize);
    }

    fn mkdir (&mut self, _req: &Request, parent: u64, name: &path::Path, _mode: u32, reply: ReplyEntry) {
        let to_create = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        match fs::create_dir(&to_create)
        {
            Ok(_) => {
                trace!("Successfully created directory {}", to_create.display());
                match to_create.symlink_metadata() {
                    Ok(md) => {
                        self.inodes.store(md.ino(), &to_create, _req.pid());
                        reply.entry(
                            &TTL,
                            &fill_file_attr(&to_create
                                            .symlink_metadata()
                                            .unwrap()),
                            0
                        );
                    },
                    Err(why) => {
                        warn!("Newly created directory {} was probably racily removed : {:?}", to_create.display(), why);
                        reply.error(why.raw_os_error().unwrap());
                    },
                }
            },
            Err(why) => {
                warn!("Could not create directory {} : {:?}", to_create.display(), why);
                reply.error(why.raw_os_error().unwrap());
            },
        }
    }

    fn rmdir (&mut self, _req: &Request, parent: u64, name: &path::Path, reply: ReplyEmpty) {
        let dir = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let md = match dir.symlink_metadata() {
            Ok(md) => md,
            Err(why)   => {
                warn!("Could not remove directory {} : {:?}", &dir.display(), why);
                reply.error(why.raw_os_error().unwrap());
                return;
            }
        };
        match fs::remove_dir(&dir) {
            Ok(_) => {
                trace!("Successfully removed directory {}", &dir.display());
                self.inodes.remove(md.ino(), Some(&dir), 0, 0);
                reply.ok();
            },
            Err(why) => {
                warn!("Could not remove directory {} : {:?}", &dir.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn opendir (&mut self, _req: &Request, _ino: u64, _flags: u32, reply: ReplyOpen) {
        // This is only useful to prevent the inodecache from forgetting some hot inode.
        trace!("Made handle to directory {}", self.inodes.resolve(_ino).display());

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        reply.opened(self.inodes.hot_files.make_handle(None, _ino), _flags);
    }

    fn readdir (&mut self, _req: &Request, ino: u64, _fh: u64, offset: u64, mut reply: ReplyDirectory) {
		use std::os::unix::fs::DirEntryExt;
        trace!("fn readdir for ino {}, at offset {}", ino, offset);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        if offset > 0 {
            if let Some(mut list) = self.dentry_cache.remove(&ino) {
                let list = list.remove(0);
                // offset - 1 instead of + 1 is for the "." and ".." added dentries that aren't in the cache. So offset + 1 - 2...
                if list.len() == {offset - 1} as usize {
                    // We're done!
                    trace!("Finalising readdir transaction with the kernel for ino {}.", ino);
                    reply.ok();
                    return;
                } else {
                    trace!("Wow this directory is huge!!! (or your buffer is tiny...)");
                    self.dentry_cache.insert(ino, list);
                }
            } else {
                warn!("This is not supposed to happen : kernel is asking for offset > 0, but dentries are not in the cache... Replying Error to the kernel...");
                // For some uncanny reason, the kernel keeps asking for ino 1 a lot of times at program startup, then stops. So this case actually happens...
                reply.error(ENOENT);
                return;
            }
        }
         if ! self.dentry_cache.contains_key(&ino) {
             trace!("Dentries for ino {} are not in the readdir cache. So let's get them from the disk.", ino);
             match fs::read_dir(self.inodes.resolve(ino)) {
                 // Found dir entries !
                 Ok(dentries) => {
                     self.dentry_cache.insert(ino, dentries.collect::<Vec<_>>());
                     trace!("Added dentries for ino {} in readdir cache.", ino);
                 },
                 // Path is invalid or protected?
                 Err(e)=> {
                     error!("{:?}",e);
                     reply.error(e.raw_os_error().unwrap());
                     return
                 },
             }
         }
        // Now get dentries from the cache.
        let dentries = match self.dentry_cache.get_mut(&ino) {
            Some(dentries) => dentries,
            None => {
                    trace!("Couldn't get dentries from readdir cache for ino {}",ino);
                    reply.error(ENOENT);
                    return
                },
            };
        // Now we can (re)start sending dentries to the kernel.
        reply.add(ino, 0, FileType::Directory, ".");
        reply.add(ino, 1, FileType::Directory, "..");// TODO : should we bother getting the parent's inode ?
        let mut count = 0;
        while count < dentries.len() {
            match dentries[count] {
                Ok(ref dentry) => {
					match dentry.file_type() {
						Ok(file_type) => {
							trace!("adding {:?} to reply with ino {} and offset {}", dentry.file_name(), dentry.ino(), count);
							// count + 2 is for "." and ".." dentries added.
							if reply.add(dentry.ino(),
								 {count + 2} as u64,
								 fuse_file_type(file_type),
								 dentry.file_name()) {
									 trace!("DirEntry buffer filled! Breaking : waiting for kernel to call back and take the rest of the dentries...");
									 break;
							}
						},
						Err(why) => {
							error!("Could not get file type of {:?} : {:?}\n We're forced to skip this entry because there is no way to reply an unknown file type to the request.", dentry.file_name(), why);
							break;
						}
					}

                },
                Err(ref e) => println!("{:?}", e),
            }
            count += 1;
        }
        debug!("Filled DirEntry buffer. Now Sending to the kernel.");
        reply.ok();
     }

    fn releasedir (&mut self, _req: &Request, _ino: u64, _fh: u64, _flags: u32, reply: ReplyEmpty) {
         // Quite straightforward as of now!
         self.inodes.hot_files.release_handle(_fh);
         trace!("Released handle {} to directory {} (inode={})", _fh, self.inodes.resolve(_ino).display(), _ino);
         reply.ok();
    }

    fn open (&mut self, _req: &Request, _ino: u64, flags: u32, reply: ReplyOpen) {
        debug!("open callback for ino {} and flags {}", _ino, flags);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let path = self.inodes.resolve(_ino);
        let read_f =
                   flags | O_RDWR as u32 == flags
                   ||
                   flags | O_RDONLY as u32 == flags;
        let write_f =
                   flags | O_RDWR as u32 == flags
                   ||
                   flags | O_WRONLY as u32 == flags;
        let append_f = flags | O_APPEND as u32 == flags;
        let truncate_f = flags | O_TRUNC as u32 == flags;

        match fs::OpenOptions::new()
                        .read(read_f)
                        .write(write_f)
                        .append(append_f)
                        .truncate(truncate_f)
                        .open(&path) {
             Ok(file) => {
                 trace!("Opened successfully {} with read={}, write={}, append={} and truncate={}", path.display(), read_f, write_f, append_f, truncate_f);
                 reply.opened(self.inodes.hot_files.make_handle(Some(file), _ino), flags);
             }
             Err(why) => {
                 warn!("Could not open file {} with read={}, write={}, append={} and truncate={} : {:?}", path.display(), read_f, write_f, append_f, truncate_f, why);
                 reply.error(why.raw_os_error().unwrap());
             }
         }
     }

    fn read (&mut self, _req: &Request, ino: u64, _fh: u64, offset: u64, _size: u32, reply: ReplyData) {
        debug!("read callback for ino {} and file handle {}, at offset {} for the size of {}", ino, _fh, offset, _size);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let mut buffer = Vec::with_capacity(_size as usize);
        buffer.resize(_size as usize, 0);
        let mut file = self.inodes.hot_files.take_file(_fh);
        if let Err(why) = file.seek(SeekFrom::Start(offset)) {
            error!("Ominous error while seeking to offset {} of ino {} : {:?}", offset, ino, why);
            self.inodes.hot_files.restore_file(_fh, file, ino);
            reply.error(why.raw_os_error().unwrap());
            return;
        }
        let mut nbytes = 0;
        while nbytes < _size as usize {
            match file.read(&mut buffer) {
                Ok(n) => if n == 0 {
                    trace!("buffer filled !");
                    break;
                } else {
                    nbytes += n;
                    trace!("Just read {} bytes !", nbytes);
                },
                Err(e) => if e.kind() == io::ErrorKind::Interrupted {
                    trace!("Hmm encountered an io::ErrorKind::Interrupted but it doesn't matter.");
                    continue;
                } else {
                    error!("read callback interrupted by {:?}", e);
                    // Should we hand in the buffer anyway ? No, for now.
                    self.inodes.hot_files.restore_file(_fh, file, ino);
                    reply.error(e.raw_os_error().unwrap());
                    return;
                }
            }
        }
        reply.data(buffer.as_slice());
        self.inodes.hot_files.restore_file(_fh, file, ino);
        debug!("Successfully sent buffer of {} bytes to the kernel", buffer.len());
    }

    fn write (&mut self, _req: &Request, ino: u64, _fh: u64, offset: u64, data: &[u8], _flags: u32, reply: ReplyWrite) {

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let mut file = self.inodes.hot_files.take_file(_fh);
        if let Err(why) = file.seek(SeekFrom::Start(offset)) {
            error!("Ominous error while seeking to offset {} of ino {} : {:?}", offset, ino, why);
            self.inodes.hot_files.restore_file(_fh, file, ino);
            reply.error(why.raw_os_error().unwrap());
            return;
        }
        match file.write(data) {
            Ok(n) => {
                trace!("Successfully wrote {} bytes to {}", n, self.inodes.resolve(ino).display());
                reply.written(n as u32);
            },
            Err(why) => {
                warn!("Could not write {} bytes to {} : {:?}", data.len(), self.inodes.resolve(ino).display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
        self.inodes.hot_files.restore_file(_fh, file, ino);
    }

    fn flush (&mut self, _req: &Request, _ino: u64, _fh: u64, _lock_owner: u64, reply: ReplyEmpty) {

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let mut file = self.inodes.hot_files.take_file(_fh);
        match file.flush() {
            Ok(_) => {
                trace!("Successfully flushed to disk file {}", self.inodes.resolve(_ino).display());
                reply.ok();
            },
            Err(what) => {
                warn!("Could not flush to disk data of file {} : {:?}", self.inodes.resolve(_ino).display(), what);
                reply.error(what.raw_os_error().unwrap());
            }
        }
        self.inodes.hot_files.restore_file(_fh, file, _ino);
    }

    fn create (&mut self, _req: &Request, parent: u64, name: &path::Path, _mode: u32, flags: u32, reply: ReplyCreate) {
        let to_create = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let read_f =
                   flags | O_RDWR as u32 == flags
                   ||
                   flags | O_RDONLY as u32 == flags;
        let write_f =
                   flags | O_RDWR as u32 == flags
                   ||
                   flags | O_WRONLY as u32 == flags;
		let append_f = flags | O_APPEND as u32 == flags;
		let truncate_f = flags | O_TRUNC as u32 == flags;
        match fs::OpenOptions::new()
                       .read(read_f)
                       .write(write_f)
                       .append(append_f)
                       .truncate(truncate_f)
                       .create_new(true)
                       .open(&to_create) {
            Ok(file) => {
                let md = match file.metadata() {
                    Ok(md) => md,
                    Err(why) => {
                        warn!("Newly created file {} was probably racily removed : {:?}", to_create.display(), why);
                        reply.error(why.raw_os_error().unwrap());
                        return;
                    },
                };
                let ino = md.ino();
                self.inodes.store(ino, &to_create, _req.pid());
                // store it into the fh cache too.

                trace!("Successfully created file {} with read={}, write={}, append={} and truncate={}", to_create.display(), read_f, write_f, append_f, truncate_f);
                reply.created(
                    &TTL,
                    &fill_file_attr(&md),
                    0, // Generation?
                    self.inodes.hot_files.make_handle(Some(file), ino),
                    flags
                );
            },
            Err(why) => {
                warn!("Could not create {} with read={}, write={}, append={} and truncate={} : {:?}", to_create.display(), read_f, write_f, append_f, truncate_f, why);
                reply.error(why.raw_os_error().unwrap());
            },
        }
    }

    fn release (&mut self, _req: &Request, _ino: u64, _fh: u64, _flags: u32, _lock_owner: u64, _flush: bool, reply: ReplyEmpty) {
        // Quite straightforward as of now!
        self.inodes.hot_files.release_handle(_fh);
        trace!("Released handle {} to file {} (inode={})", _fh, self.inodes.resolve(_ino).display(), _ino);
        reply.ok();
    }

    fn rename (&mut self, _req: &Request, parent: u64, name: &path::Path, _newparent: u64, _newname: &path::Path, reply: ReplyEmpty) {
        let old_path = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };
        let new_path = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);


        match fs::rename(&old_path, &new_path) {
            Ok(_) => {
                match new_path.symlink_metadata() {
                    Ok(md) => {
                        trace!("Successfully renamed {} to {}", old_path.display(), new_path.display());
                        self.inodes.remove(md.ino(), Some(&old_path), 0, 0);
                        self.inodes.store(md.ino(), &new_path, _req.pid());
                        reply.ok();
                    },
                    Err(why) => {
                        warn!("Path renamed from {} into {} could not be queried for metadata : {:?}", old_path.display(), new_path.display(), why);
                        reply.error(why.raw_os_error().unwrap());
                    }
                }
            },
            Err(why) => {
                warn!("Could not rename {} into {} : {:?}", old_path.display(), new_path.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn link (&mut self, _req: &Request, _ino: u64, _newparent: u64, _newname: &path::Path, reply: ReplyEntry) {
        let first_path = self.inodes.resolve(_ino);
        let next_path = match self.name2original(_newname, _newparent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        if first_path == next_path {
            reply.error(EEXIST);
            return;
        }
        match fs::hard_link(&first_path, &next_path) {
            Ok(_) => {
                trace!("Successfully created link {} based on {}", next_path.display(), first_path.display());
                match next_path.symlink_metadata() {
                    Ok(md) => reply.entry(&TTL,
                        &fill_file_attr(&md),
                        0
                    ),
                    Err(what) => {
                        warn!("It seems the link just created ({}) could not be queried for metadata. Was it removed in an race condition ? : {:?}", next_path.display(), what);
                        reply.error(what.raw_os_error().unwrap());
                    }
                }
            },
            Err(why) => {
                warn!("Could not create link {} based on {} (inode = {}) : {:?}", next_path.display(), first_path.display(), _ino, why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn unlink (&mut self, _req: &Request, parent: u64, name: &path::Path, reply: ReplyEmpty) {
        let file = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let md = match file.symlink_metadata() {
            Ok(md) => md,
            Err(why) => {
                warn!("Could not remove file {} : {:?}", &file.display(), why);
                reply.error(why.raw_os_error().unwrap());
                return;
            }
        };
        match fs::remove_file(&file) {
            Ok(_) => {
                trace!("Successfully removed file {}", &file.display());
                self.inodes.remove(md.ino(), Some(&file), 0, 0);
                reply.ok();
            },
            Err(why) => {
                warn!("Could not remove file {} : {:?}", &file.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn mknod (&mut self, _req: &Request, parent: u64, name: &path::Path, _mode: u32, _rdev: u32, reply: ReplyEntry) {
        use nix::sys::stat;

        let node = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let kind = stat::SFlag::from_bits_truncate(_mode as libc::mode_t);
        let perm = stat::Mode::from_bits_truncate(_mode as libc::mode_t);

        match stat::mknod(&node, kind, perm, _rdev as nix::sys::stat::dev_t) {
            Ok(_) => {
                trace!("Successfully created node {} as a {:?} with permissions {:?}", node.display(), kind, perm);
                match node.symlink_metadata() {
                    Ok(md) => {
                        self.inodes.store(md.ino(), &node, _req.pid());
                        reply.entry(&TTL,
                        &fill_file_attr(&md),
                        0// Generation?
                        );
                    },
                    Err(what) => {
                        warn!("It seems the node just created ({}) could not be queried for metadata. Was it removed in an race condition ? : {:?}", node.display(), what);
                        reply.error(what.raw_os_error().unwrap());
                    }
                }
            }
            Err(why) => {
                warn!("Could not create node {} as a {:?} with permissions {:?} : {:?}", node.display(), kind, perm, why);
                reply.error(why.errno() as i32);
            }
        }
    }

    fn getattr (&mut self, _req: &Request, _ino: u64, reply: ReplyAttr) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        match path.symlink_metadata() {
            Ok(md) => {
                trace!("Successfully got attributes for {}", path.display());
                reply.attr(&TTL, &fill_file_attr(&md));
            }
            Err(why) => {
                warn!("Could not get attributes for {} : {:?}", path.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn setattr (&mut self, _req: &Request, _ino: u64, _mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, _size: Option<u64>, _atime: Option<Timespec>, _mtime: Option<Timespec>, _fh: Option<u64>, _crtime: Option<Timespec>, _chgtime: Option<Timespec>, _bkuptime: Option<Timespec>, _flags: Option<u32>, reply: ReplyAttr) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        if let Some(mode) = _mode {
            trace!("Setting mode {}", mode);
            let perm = fs::Permissions::from_mode(mode);
            if let Err(why) = fs::set_permissions(&path, perm) {
                warn!("Could not set attributes for {} : {:?}", path.display(), why);
                reply.error(why.raw_os_error().unwrap());
                return;
            }
        }

        if let Some(size) = _size {
            match fs::OpenOptions::new().write(true).open(&path){
                Ok(file) => {
                    trace!("Setting length to {}", size);
                    if let Err(why) = file.set_len(size) {
                        warn!("Could not set length for {} : {:?}", path.display(), why);
                        reply.error(why.raw_os_error().unwrap());
                        return;
                    }
                },
                Err(why) => {
                    warn!("Could not open {} to set its length : {:?}", path.display(), why);
                    reply.error(why.raw_os_error().unwrap());
                    return;
                }
            }
        }

        if _uid.is_some() || _gid.is_some() {
            let md = match path.symlink_metadata() {
                Ok(md) => md,
                Err(why) => {
                    warn!("Could not open {} to set uid/gid : {:?}", path.display(), why);
                    return;
                }
            };
            let uid;
            if _uid.is_none() {
                uid = md.uid();
            } else {
                uid = _uid.unwrap();
            }
            let gid;
            if _gid.is_none() {
                gid = md.gid();
            } else {
                gid = _gid.unwrap();
            }
            unsafe {
                if libc::chown(
                // TODO : with nix path ?
                    path.as_os_str().to_str().unwrap().as_ptr() as *const libc::c_char,
                    uid as libc::uid_t,
                    gid as libc::gid_t
                ) != 0 {
                    let e = nix::errno::errno();
                    warn!("Impossible to change uid and gid of {} : error {}", path.display(), e);
                    reply.error(e);
                    return;
                }
            }
        }

        if _atime.is_some() || _mtime.is_some() {
            let atime;
            let mtime;
            if _atime.is_none() { atime = 0;} else {
                atime = _atime.unwrap().sec;
            }
            if _mtime.is_none() { mtime = 0;} else {
                mtime = _mtime.unwrap().sec;
            }
            match utime::set_file_times(&path, atime as u64, mtime as u64) {
                Ok(_) => {
                    trace!("Set atime to {} and mtime to {} for path {}", atime, mtime, path.display());
                }
                Err(why) => {
                    warn!("Could not set atime to {} and mtime to {} for path {} : {:?}", atime, mtime, path.display(), why);
                    reply.error(why.raw_os_error().unwrap());
                    return;
                }
            }
        }

        if _bkuptime.is_some() || _chgtime.is_some() || _crtime.is_some() || _flags.is_some() {
            reply.error(ENOSYS);
            return;
        }

        // return what is actually on disc.
        match path.symlink_metadata() {
            Ok(md) => {
                trace!("Successfully got newly set attributes for {}", path.display());
                reply.attr(&TTL, &fill_file_attr(&md));
            }
            Err(why) => {
                warn!("Could not get attributes for {} : {:?}", path.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn access (&mut self, _req: &Request, _ino: u64, _mask: u32, reply: ReplyEmpty) {
        let path = self.inodes.resolve(_ino);
        
        match self.u_access(_req, &path, _mask) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    // TODO : implement an optional absolute-path anonymisation for the target, by making it relative to the link?
    fn symlink (&mut self, _req: &Request, parent: u64, name: &path::Path, _link: &path::Path, reply: ReplyEntry) {
        let name = match self.name2original(name, parent, _req, WRITE) {
            Ok(path) => path,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let mut virtual_name = path::PathBuf::from(&self.virtual_path);
        virtual_name.push(
                        name.strip_prefix(&self.base_path)
                            .unwrap()
                        );
        match symlink(&_link, &name) {
            Ok(_) => {
                trace!("Successfully created symlink {} pointing to {}", name.display(), _link.display());
                match name.symlink_metadata() {
                    Ok(md) => {
                        self.inodes.store(md.ino(), &name, _req.pid());
                        reply.entry(
                            &TTL,
                            &fill_file_attr(&md),
                            0
                        );
                    },
                    Err(what) => {
                        warn!("It seems the symlink just created ({}) could not be queried for metadata. Was it removed in an race condition ? : {:?}", name.display(), what);
                        reply.error(what.raw_os_error().unwrap());
                    }
                }
            },
            Err(why) => {
                warn!("Could not create symlink {} pointed to {} : {:?}", name.display(), _link.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }

    fn readlink (&mut self, _req: &Request, ino: u64, reply: ReplyData) {
        let symln = self.inodes.resolve(ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        match symln.read_link() {
            Ok(file) => {
                trace!("Sending {} as response to readlink", file.display());
                reply.data(file.as_os_str().as_bytes());
            },
            Err(why) => {
                warn!("Could not read symlink pointed to by {} : {:?}", symln.display(), why);
                reply.error(why.raw_os_error().unwrap());
            }
        }
    }
// FIXME: We have to improve the libfuse implementation in order to be able to debug and use list and get xattr functions...
    fn listxattr (&mut self, _req: &Request, _ino: u64, reply: ReplyEmpty) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        // The libc way is racy: to avoid hypothetical cases where xattr change between the two calls, let's loop as long as the result is incoherent (hopefully only once!)
        loop {
			let res = path.with_nix_path( |cstr| {
				unsafe {
					libc::llistxattr(
						cstr.as_ptr(),
						0 as *mut libc::c_char,
						0
					)
				}
			}).unwrap();
			match res {
				-1 => {
					let e = nix::errno::errno();
					warn!("Could not list extended attributes for {} : error {}", path.display(), e);
					reply.error(e);
					break;
				},
				0 => {
					trace!("{} has got no extended attribute", path.display());
					//FIXME the callback should not take a ReplyEmpty !
					reply.ok();
					break;
				}
				len @ _ => {
					let mut list = Vec::with_capacity(len as usize);
					unsafe{list.set_len(len as usize);}
					let list = list.as_mut_ptr();
					let res = path.with_nix_path( |cstr| {
						unsafe{
							libc::llistxattr(
								cstr.as_ptr(),
								list as *mut libc::c_char,
								len as libc::size_t
							)
						}
					}).unwrap();
					if res == len {
						trace!("Retrieved list of extended attributes for {}, but could not send them to fuse because the callback doesn't care...", path.display());
						reply.ok();
						break;
					} else {
						let e = nix::errno::errno();
						warn!("Something went wrong when getting list of extended attributes for {}. Probably a race condition : retrying now! (error code {})", path.display(), e);
					}
				}
			}
        }
    }

    fn getxattr (&mut self, _req: &Request, _ino: u64, name: &ffi::OsStr, reply: ReplyData) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        // loop to avoid race conditions (cf listxattr)
        loop {
			let res = path.with_nix_path( |cstr| {
				unsafe {
					libc::lgetxattr(
						cstr.as_ptr(),
						name.to_str().unwrap().as_ptr() as *const libc::c_char,
						0 as *mut libc::c_void,
						0
					)
                }
			}).unwrap();
			match res {
				len if len >= 0 => {
					let mut value = Vec::with_capacity(len as usize);
					unsafe {value.set_len(len as usize);}
					let value = value.as_mut_ptr();
					let res = path.with_nix_path( |cstr| {
						unsafe{
							libc::lgetxattr(
								cstr.as_ptr(),
								name.to_str().unwrap().as_ptr() as *const libc::c_char,
								value as *mut libc::c_void,
								len as libc::size_t
							)
						}
					}).unwrap();
					if res != len {
						let e = nix::errno::errno();
						error!("Extended attribute under name {:?} has changed during the racy operation (for file {}) : retrying now until coherent result ! (error code {})", name, path.display(), e);
					} else {
						trace!("Happily retrieved value for extended attribute under name {:?} on file {}", name, path.display());
						reply.data(
							unsafe{slice::from_raw_parts(value, len as usize)}
						);
						return;
					}
				}
				_ => {
					let e = nix::errno::errno();
					warn!("Could not retrieve any value for name {:?} : probably because the name does not exist for file {} : error number {}", name, path.display(), e);
					reply.error(e);
					return;
				}
			}
        }
    }
    // Altough list and get xattr aren't stable, set and get are quite ok !
    fn setxattr (&mut self, _req: &Request, _ino: u64, name: &ffi::OsStr, value: &[u8], _flags: u32, _position: u32, reply: ReplyEmpty) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        //What's the use of _position ???
        trace!("_position = {:?}", _position);
        
		if path.with_nix_path( |cstr| {
			unsafe{
				libc::lsetxattr(
					cstr.as_ptr(),
					name.to_str().unwrap().as_ptr() as *const libc::c_char,
					value.as_ptr() as *mut libc::c_void,
					value.len() as libc::size_t,
					_flags as libc::c_int
				)
			}
		}).unwrap() == 0 {
			trace!("Successfully set extended attribute {:?} under name {:?} for file {}", value, name, path.display());
			reply.ok();
		} else {
			let e = nix::errno::errno();
			warn!("Could not set value {:?} for name {:?} for file {} : error number {}", value, name, path.display(), e); // value.to_string
			reply.error(e);
		}
    }

    fn removexattr (&mut self, _req: &Request, _ino: u64, name: &ffi::OsStr, reply: ReplyEmpty) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);
                
        if path.with_nix_path( |cstr| {
			unsafe{
				libc::removexattr(
					cstr.as_ptr(),
					name.to_str().unwrap().as_ptr() as *const libc::c_char,
				)
			}
		}).unwrap() == 0 {
			trace!("Successfully removed extended attribute under name {:?} for file {}", name, path.display());
			reply.ok();
		} else {
			let e = nix::errno::errno();
			warn!("Could not remove extended attribute under name {:?} for file {}", name, path.display());
			reply.error(e);
		}
    }

    fn fsync (&mut self, _req: &Request, ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        let file = self.inodes.hot_files.take_file(_fh);
        if _datasync {
            trace!("Syncing data (not metadata) of file {}", self.inodes.resolve(ino).display());
            if let Err(e) = file.sync_data() {
                warn!("Could not fsync {} : {:?}", self.inodes.resolve(ino).display(), e);
                reply.error(e.raw_os_error().unwrap());
                self.inodes.hot_files.restore_file(_fh, file, ino);
                return;
            };
            self.inodes.hot_files.restore_file(_fh, file, ino);
            reply.ok();
        } else {
            trace!("Syncing data and metadata of file {}", self.inodes.resolve(ino).display());
            if let Err(e) = file.sync_all() {
                warn!("Could not fsync {} : {:?}", self.inodes.resolve(ino).display(), e);
                reply.error(e.raw_os_error().unwrap());
                self.inodes.hot_files.restore_file(_fh, file, ino);
                return;
            };
            self.inodes.hot_files.restore_file(_fh, file, ino);
            reply.ok();
        }
    }

    fn statfs (&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let path = self.inodes.resolve(_ino);

        // UserMap restores the fsuid/fsgid by Dropping.
        let user_token = self.userprelude(_req);

        // TODO : replace this unsafe block by a call to the nix implementation ?
        unsafe {
            let mut stats: libc::statfs = mem::uninitialized();
            if libc::statfs(path.as_os_str().to_str().unwrap().as_ptr() as *const libc::c_char, &mut stats as *mut libc::statfs) == -1 {
                let e = nix::errno::errno();
                warn!("Impossible to statfs {} : error code {}", path.display(), e);
                reply.error(e);
                return;
            }
            reply.statfs(
                stats.f_blocks,
                stats.f_bfree,
                stats.f_bavail,
                stats.f_files,
                stats.f_ffree,
                stats.f_bsize as u32,
                stats.f_namelen as u32,
                stats.f_frsize as u32
            );
        }
    }
}
