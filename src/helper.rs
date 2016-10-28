// Just functions that may be useful to many modules.

use std::fs::Metadata;
use std::fs::FileType as StdFileType;
use std::os::unix::fs::{MetadataExt, FileTypeExt, PermissionsExt};
use time::Timespec;
use fuse::{FileAttr, FileType};

pub fn fuse_file_type(file_type : StdFileType) -> FileType {
    if file_type.is_dir() == true {
            FileType::Directory
        } else if file_type.is_file() == true {
            FileType::RegularFile
        } else if file_type.is_symlink() == true {
            FileType::Symlink
        } else if file_type.is_block_device() == true {
            FileType::BlockDevice
        } else if file_type.is_fifo() == true {
            FileType::NamedPipe
        } else if file_type.is_char_device() == true {
            FileType::CharDevice
        } else {
            // Sockets aren't supported apparently.
            FileType::RegularFile
        }
}

pub fn fill_file_attr(md : &Metadata) -> FileAttr {
    FileAttr{
        ino : md.ino(),
        size : md.size(),
        blocks : md.blocks(),
        atime : Timespec{ sec : md.atime(), nsec : md.atime_nsec() as i32, },
        mtime : Timespec{ sec : md.mtime(), nsec : md.mtime_nsec() as i32, },
        ctime : Timespec{ sec : md.ctime(), nsec : md.ctime_nsec() as i32, },
        crtime : Timespec{ sec : 0, nsec : 0 }, //unavailable on Linux...
        kind : fuse_file_type(md.file_type()),
        perm : md.permissions().mode() as u16,
        nlink : md.nlink() as u32,
        uid : md.uid(),
        gid : md.gid(),
        rdev : md.rdev() as u32,
        flags : 0,
    }
}

use std::path::Path;
use fuse::Request;
use libc::EACCES;
use mirrorfs::MirrorFS;
use std::ops::Shl;
// Allows or denies access according to DAC (user/group permissions).
impl MirrorFS {
	pub fn u_access(&self, _req: &Request, path: &Path, _mask: u32) -> Result<(), i32> {
		let (uid, gid) = self.usermap(_req);
		
		if self.settings.fullaccess.contains(&uid) {
			trace!("User {} is almighty so access is ok!", uid);
			return Ok(());
		}
		
		match path.symlink_metadata() {
			Ok(md) => {
					if uid == md.uid() {
					if md.permissions().mode() | _mask.shl(6) == md.permissions().mode() {
						trace!("Access request {:b} as user {} on path {} is ok", _mask.shl(6), uid, path.display());
						return Ok(());
					} else {
						trace!("Access request as user isn't ok! Request was {:b}, Permissions were {:b}", _mask.shl(6), md.permissions().mode());
						return Err(EACCES);
					}
				} else if gid == md.gid() {
					if md.permissions().mode() | _mask.shl(3) == md.permissions().mode() {
						trace!("Access request {:b} as group member of {} on path {} is ok", _mask.shl(3), gid, path.display());
						return Ok(());
					} else {
						trace!("Access request as group member isn't ok! Request was {:b}, Permissions were {:b}", _mask.shl(3), md.permissions().mode());
						return Err(EACCES);
					}
				} else {
					if md.permissions().mode() | _mask == md.permissions().mode() {
						trace!("Access request {:b} as \"other\" on path {} is ok",  _mask, path.display());
						return Ok(());
					} else {
						trace!("Access request as \"other\" isn't ok! Request was {:b}, Permissions were {:b}", _mask, md.permissions().mode());
						return Err(EACCES);
					}
				}
			},
			Err(why) => {
				warn!("Could not get metadata to file {} : {:?}", path.display(), why);
				return Err(why.raw_os_error().unwrap());
			}
		}
	}
}

