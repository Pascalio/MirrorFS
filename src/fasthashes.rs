use std::collections;
use std::hash::BuildHasherDefault;
use std::hash::Hash;
use fnv::FnvHasher;

pub type FastMap<T, U> where T : Hash + Eq = collections::HashMap<T, U, BuildHasherDefault<FnvHasher>>;
pub type FastSet<T> where T : Hash + Eq = collections::HashSet<T, BuildHasherDefault<FnvHasher>>;

pub trait FastHasher<T> {
	fn with_capacity(capacity: usize) -> Self;
}

impl<T, U> FastHasher<T> for FastMap<T, U> where T : Hash + Eq {
	fn with_capacity(capacity: usize) -> FastMap<T, U> {
		collections::HashMap::<T, U, BuildHasherDefault<FnvHasher>>::with_capacity_and_hasher(capacity, BuildHasherDefault::<FnvHasher>::default())
	}
}
impl<T> FastHasher<T> for FastSet<T> where T : Hash + Eq {
	fn with_capacity(capacity: usize) -> FastSet<T> {
		collections::HashSet::<T, BuildHasherDefault<FnvHasher>>::with_capacity_and_hasher(capacity, BuildHasherDefault::<FnvHasher>::default())
	}
}
