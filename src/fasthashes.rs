use std::collections;
use std::hash::BuildHasherDefault;
use fnv::FnvHasher;

pub type FastMap = collections::HashMap<u32, u32, BuildHasherDefault<FnvHasher>>;
pub type FastMap64 = collections::HashMap<u64, u64, BuildHasherDefault<FnvHasher>>;
pub type FastSet = collections::HashSet<u32, BuildHasherDefault<FnvHasher>>;
pub type FastSet64 = collections::HashSet<u64, BuildHasherDefault<FnvHasher>>;
