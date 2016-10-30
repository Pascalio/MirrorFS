use fuse::Request;
use mirrorfs::MirrorFS;
use capabilities::{Capabilities, Capability, Flag};

pub type Uid = u32;
pub type Gid = u32;

#[cfg(feature="enable_unsecure_features")]
pub struct UserMap {
	o_user : Uid,
	o_group : Gid,
	n_user : Uid,
	n_group : Gid,
	#[allow(dead_code)]
	caps : Option<CapToken>,
}
#[cfg(not(feature="enable_unsecure_features"))]
pub struct UserMap {
	o_user : Uid,
	o_group : Gid,
	n_user : Uid,
	n_group : Gid,
}

impl Drop for UserMap {
	fn drop(&mut self) {
		if self.n_user != self.o_user {
			trace!("Trying to restore FS UID from the requesting UID {} to our own UID {} (it should probably work.)", self.n_user, self.o_user);
			unsafe {
				syscall!(SETFSUID, self.o_user);
			}
		}
		if self.n_group != self.o_group {
			trace!("Trying to restore FS GID from the requesting GID {} to our own GID {} (it should probably work.)", self.n_group, self.o_group);
			unsafe {
				syscall!(SETFSGID, self.o_group);
			}
		}
	}
}

pub struct CapToken {
	caps : Vec<Capability>
}

impl Drop for CapToken {
	fn drop(&mut self) {
		let mut caps = Capabilities::from_current_proc().unwrap();
		caps.update(&self.caps, Flag::Effective, false);
		caps.apply().unwrap();
		trace!("Took back capability set {}", caps);
	}
}

impl MirrorFS {
	// TODO: Possibly, mimic capabilities set of calling process (ie, non-0 uids with capabilities).
    pub fn userprelude(&self, req: &Request) -> UserMap {
        let (user, group) = self.usermap(req);

		let o_user;
		let o_group;
		        
		if user != self.settings.uid {
			if !self.settings.has_cap(Capability::CAP_SETUID) {
				trace!("Cannot set fs UID to {}: no user embodiment (you need CAP_SETUID on the fuse implementation)", user);
				o_user = user;
			} else {
				trace!("Trying to switch FS UID to the UID of the requesting process ({}) -- It probably will work but we have no way to check, except for looking at the result on the FS...", user);
				o_user = unsafe {
					syscall!(SETFSUID, user)
				} as Uid;
			}
		} else {
			trace!("No need to embody requesting user.");
			o_user = user;
		}
		if group != self.settings.gid {
			if !self.settings.has_cap(Capability::CAP_SETGID) {
				trace!("Cannot set fs GID to {}: no user embodiment (you need CAP_SETGID on the fuse implementation)", user);
				o_group = group;
			} else {
				trace!("Trying to switch FS GID to the GID of the requesting process ({}) -- It probably will work but we have no way to check, except for looking at the result on the FS...", user);
				o_group = unsafe {
					syscall!(SETFSGID, group)
				} as Gid;
			}
		} else {
			trace!("No need to embody requesting group.");
			o_group = group;
		}
		
		#[cfg(feature="enable_unsecure_features")] {
			// TODO: optimize for regular case where no full access.
			let cap_token = if self.settings.fullaccess.contains(&user) {
				trace!("Giving {} full access!", user);
				Some(self.set_cap(&[Capability::CAP_FOWNER, Capability::CAP_DAC_OVERRIDE, Capability::CAP_MKNOD, Capability::CAP_CHOWN, Capability::CAP_SETFCAP]))
			} else {
				trace!("Not giving {} full access.", user);
				None
			};
			trace!("Operation will be performed with this capability set : {}", Capabilities::from_current_proc().unwrap());
			UserMap {
				o_user : o_user,
				o_group : o_group,
				n_user : user,
				n_group : group,
				caps : cap_token,
			}
		}
		#[cfg(not(feature="enable_unsecure_features"))] {
			trace!("Operation will be performed with this capability set : {}", Capabilities::from_current_proc().unwrap());
			UserMap {
				o_user : o_user,
				o_group : o_group,
				n_user : user,
				n_group : group,
			}
		}
    }

    pub fn usermap(&self, req: &Request) -> (Uid, Gid) {
        let mut calling_u = req.uid();
        let mut calling_g = req.gid();
        #[cfg(feature="enable_unsecure_features")] {
			if self.settings.user_map.is_empty() {
				trace!("No user mapping on requests.");
			} else {
				if let Some(mapped_u) = self.settings.user_map.get(&calling_u) {
					trace!("Mapping uid {} to {}.", calling_u, mapped_u);
					calling_u = *mapped_u;
				}
				if let Some(mapped_g) = self.settings.group_map.get(&calling_g) {
					trace!("Mapping gid {} to {}", calling_g, mapped_g);
					calling_g = *mapped_g;
				}
			}
		}
		(calling_u, calling_g)
    }
    
    pub fn set_cap(&self, caps : &[Capability]) -> CapToken {
		let mut added_caps : Vec<Capability> = Vec::new();
		for cap in caps.iter() {
			if self.settings.has_cap(*cap) {
				let mut caps = Capabilities::from_current_proc().unwrap();
				caps.update(&[*cap], Flag::Effective, true);
				caps.apply().expect("Applying caps should not fail!");
				trace!("Just set process wide capability {}", cap);
				added_caps.push(*cap);
			} else {
				warn!("The capability {} is not permitted, action may fail!", cap);
			}
		}
		CapToken{caps: added_caps}
	}
}
