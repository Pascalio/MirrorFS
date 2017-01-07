# MirrorFS
An implementation of a userland secure Bind filesystem, written in Rust, on top of [rust-fuse.] (https://github.com/zargony/rust-fuse)
It is designed to leverage modern Linux technologies, such as capabilities and FSUID/FSGID, which makes it very Linux specific. (But if you come up with a nice patch to some other platform's technologies, it will probably not be rejected.)  

###Features
* basic mirroring, as bind mount, but from userland.
* use of the relevant capabilities to embody the requesting user, so as to perform disk access "as them" and not as the user running the filesystem (necessary for anything happening in userland!)
* unsecure features which have to be opted in at compile time (these features were made for experimenting with them, they are not useful in the long run and present obvious security breach in a system) :
  - arbitrary (defined at mount time) user and group mapping, letting Alice access as if she were Tom, basicaly.
  - giving a list of users full access to the files, overriding (for them only) all form of DAC security -- as if they were root, basicaly.  

####Technical features
- fuse implementation based on inodes and not on paths
- inode cache
- deprivileged root (dropping unneeded capabilities)
- large spectrum of verbosity: from quiet to extremely verbose, for the curious (or the debugging one)

###Stage Beta
*To do for release 1.0.0 :*  
- bug tracking...  
- optimizazions  
- cleaning up some "use" and "extern crate"...
