extern crate fuse;
extern crate libc;
extern crate time;
#[macro_use]
extern crate log;
extern crate simplelog;
#[macro_use]
extern crate p_macro;
extern crate nix;
extern crate utime;
#[macro_use]
extern crate syscall;
#[macro_use]
extern crate clap;
extern crate capabilities;
extern crate users;

// Own modules
mod mirrorfs;
mod inodecache;
mod helper;
mod filehandles;
mod user;

use clap::{App, AppSettings};
use simplelog::{FileLogger, SimpleLogger, CombinedLogger, LogLevelFilter};
use std::fs::OpenOptions;
use std::collections::{HashMap, HashSet};
use capabilities::*;
use users::{get_current_uid, get_current_gid, get_user_by_name, get_group_by_name};
// Own namespaces
use mirrorfs::MirrorFS;

// Do not forget to have libfuse-dev and libcap-dev installed to compile on Linux!


fn main () {
	let cla = load_yaml!("cla.yml");
	let args = App::from_yaml(cla)
					.setting(AppSettings::UnifiedHelpMessage) // Do not separate "flags" from "options"
					.author(crate_authors!())
					.version(crate_version!())
					.get_matches();
    let mountpoint = args.value_of("DST").unwrap();
    let origin = args.value_of("SRC").unwrap();
    let verbosity = if args.is_present("quiet") {LogLevelFilter::Off} else {
		match args.value_of("verbosity").unwrap()/* field has a default value, so unwrapping is safe*/ {
			"Trace" => LogLevelFilter::Trace,
			"Debug" => LogLevelFilter::Debug,
			"Info" => LogLevelFilter::Info,
			"Warn" => LogLevelFilter::Warn,
			"Error" => LogLevelFilter::Error,
			_ => LogLevelFilter::Off,
		}
    };
    
    //TODO : asynchronous logging framework...
    let _ = CombinedLogger::init(
        vec![
        SimpleLogger::new(verbosity),
        FileLogger::new(LogLevelFilter::Trace, OpenOptions::new().create(true).write(true).truncate(true).open("log").unwrap())
        ]
    );
    info!{"Logging's up"};
    
    // Check what capabilities we may use, and drop all needless ones.
    let mut caps = Capabilities::from_current_proc().unwrap();
	// Check for all usable caps.
	let chown_cap = caps.check(Capability::CAP_CHOWN, Flag::Permitted); // for chown obviously
	let fowner_cap = caps.check(Capability::CAP_FOWNER, Flag::Permitted); // for extended attributes; Note: if setfsuid did not unset CAP_FOWNER, this (process-wide) capability would make a huge race condition privilege escalation bug. FIXME: check with non-zero uid which has received capabilities.
	let setfcap_cap = caps.check(Capability::CAP_SETFCAP, Flag::Permitted); // for setting file capabilities. FIXME: cf fowner_cap...
	let mknod_cap = caps.check(Capability::CAP_MKNOD, Flag::Permitted); // for mknod only in case of neither regular file, nor FIFO, nor Unix domain socket
	let dac_override_cap = caps.check(Capability::CAP_DAC_OVERRIDE, Flag::Permitted); // used by the "full-access" option
	let fsuid_cap = caps.check(Capability::CAP_SETUID, Flag::Permitted); // for every fs operation on the behalf of another user.
	let fsgid_cap = caps.check(Capability::CAP_SETGID, Flag::Permitted); // for every fs operation on the behalf of another user.
	//Keep only what's needed.
	//TODO: drop based on cli options as well.
	caps.reset_all();
	if  fsuid_cap {
		caps.update(&[Capability::CAP_SETUID], Flag::Permitted, true);
		caps.update(&[Capability::CAP_SETUID], Flag::Effective, true);// This might be used extremely often, so we keep it effective. Is it wise ?
	}
	if fsgid_cap {
		caps.update(&[Capability::CAP_SETGID], Flag::Permitted, true);
		caps.update(&[Capability::CAP_SETGID], Flag::Effective, true);// This might be used extremely often, so we keep it effective. Is it wise ?
	}
	if chown_cap {
		caps.update(&[Capability::CAP_CHOWN], Flag::Permitted, true);
	}
	if fowner_cap {
		caps.update(&[Capability::CAP_FOWNER], Flag::Permitted, true);
	}
	if setfcap_cap {
		caps.update(&[Capability::CAP_SETFCAP], Flag::Permitted, true);
	}
	if mknod_cap {
		caps.update(&[Capability::CAP_MKNOD], Flag::Permitted, true);
	}
	if dac_override_cap {
		caps.update(&[Capability::CAP_DAC_OVERRIDE], Flag::Permitted, true);
	}
	 //Apply the restricted Capability set.
	let caps_res = caps.apply();
	let new_caps = Capabilities::from_current_proc().unwrap();
	match caps_res {
		Ok(_) => debug!("These are the capabilities set for the filesystem implementation {}", &new_caps),
		Err(why) => warn!("Could not drop capabilities... {:?} These are the capabilities permitted for the process {}", why, &new_caps),
	}
	
	// Build optional map of users who may override DAC, thus getting full access to any file.
	let mut fullaccess_set : HashSet<u32>;
	if let Some(users) = args.values_of("fullaccess") {
		if fowner_cap && dac_override_cap {
			fullaccess_set = HashSet::new(); // TODO: optimize with capacity...
			for a_user in users {
				if let Some(u) = get_user_by_name(a_user) {
					fullaccess_set.insert(u.uid());
					info!("Giving full access to uid {}", u.uid());
				} else {
					error!("User name {} is not valid. Not giving it full access.", a_user);
				}
			}
		} else {
			fullaccess_set = HashSet::with_capacity(0);
			error!("The CAP_FOWNER CAP_DAC_OVERRIDE capabilities are needed in order to be able to give certain users full access. Option is therefore dropped.");
		}
	} else {
		fullaccess_set = HashSet::with_capacity(0);
	}
    
    // Build optional user map.
    let no_maps = args.occurrences_of("usermap") as usize;// TODO: or inline ??
    let mut user_maps : HashMap<u32, u32> = HashMap::with_capacity(no_maps); // TODO: Other hasher.
    if let Some(maps) = args.values_of("usermap") {
		if !fsuid_cap {error!("We lack the CAP_SETUID capability. So user mapping is likely to fail in most cases!");}
		let mut second_arg = false; // Any easier way in clap ??
		let mut first_skip = false;
		let mut uid_cached = 0;
		for a_user in maps {
			if second_arg {
				second_arg = false; // Prepare for next iteration.
				if first_skip {continue;} // if first arg was invalid and hence skipped, we skip the second one as well.
				if let Some(u) = get_user_by_name(a_user) {
					user_maps.entry(uid_cached).or_insert(u.uid());
					info!("Mapping uid {} to uid {}", uid_cached, u.uid());
				} else {
					error!("User name {} is not valid. Not mapping to it.", a_user);
				}
				
			} else {
				second_arg = true; // Prepare for next iteration.
				if let Some(u) = get_user_by_name(a_user) {
					uid_cached = u.uid();
				} else {
					first_skip = true; // Invalid user name, so we skip the pair.
					error!("User name {} is not valid. Not mapping the pair.", a_user);
				}
			}
		}
	}
    // Build optional group map.
    let no_maps = args.occurrences_of("groupmap") as usize;// TODO: or inline ??
    let mut group_maps : HashMap<u32, u32> = HashMap::with_capacity(no_maps); // TODO: Other hasher.
    if let Some(maps) = args.values_of("groupmap") {
		if !fsgid_cap {error!("We lack the CAP_SETGID capability. So group mapping is likely to fail in most cases!");}
		let mut second_arg = false; // Any easier way in clap ??
		let mut first_skip = false;
		let mut gid_cached = 0;
		for a_group in maps {
			if second_arg {
				second_arg = false; // Prepare for next iteration.
				if first_skip {continue;} // if first arg was invalid and hence skipped, we skip the second one as well.
				if let Some(g) = get_group_by_name(a_group) {
					group_maps.entry(gid_cached).or_insert(g.gid());
					info!("Mapping gid {} to gid {}", gid_cached, g.gid());
				} else {
					error!("Group name {} is not valid. Not mapping to it.", a_group);
				}
				
			} else {
				second_arg = true; // Prepare for next iteration.
				if let Some(g) = get_group_by_name(a_group) {
					gid_cached = g.gid();
				} else {
					first_skip = true; // Invalid group name, so we skip the pair.
					error!("Group name {} is not valid. Not mapping the pair.", a_group);
				}
			}
		}
	}

    let fs = MirrorFS::new(
		origin, 
		mountpoint,
		get_current_uid(),
		get_current_gid(),
		user_maps,
		group_maps,
		fullaccess_set,
		new_caps
	);
    fs.mount(&mountpoint);
}
