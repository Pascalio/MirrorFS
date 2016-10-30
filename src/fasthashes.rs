use std::collections;
use std::hash::BuildHasherDefault;
use fnv::FnvHasher;

pub type FastMap<T, U> = collections::HashMap<T, U, BuildHasherDefault<FnvHasher>>;
pub type FastSet<T> = collections::HashSet<T, BuildHasherDefault<FnvHasher>>;
