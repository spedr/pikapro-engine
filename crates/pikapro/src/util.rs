//! Small allocation-free containers used in hot paths.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::{Deref, DerefMut};

/// A fixed-capacity vector stored inline. `T: Copy + Default` keeps it
/// trivially copyable and free of `unsafe`.
#[derive(Clone, Copy)]
pub struct StackVec<T: Copy + Default, const N: usize> {
    items: [T; N],
    len: u8,
}

impl<T: Copy + Default, const N: usize> StackVec<T, N> {
    /// An empty vector.
    pub fn new() -> Self {
        const { assert!(N <= u8::MAX as usize) };
        StackVec { items: [T::default(); N], len: 0 }
    }

    /// Appends `item`. Panics if full: capacities are fixed by game rules, so
    /// overflowing one is a logic error, not a runtime condition.
    #[inline]
    pub fn push(&mut self, item: T) {
        assert!((self.len as usize) < N, "StackVec capacity {N} exceeded");
        self.items[self.len as usize] = item;
        self.len += 1;
    }

    /// The element at `index` of the whole capacity, whatever the length:
    /// one bound check (against `N`, a constant) where `self[index]` makes
    /// two (the length against `N`, then the index against the length). For
    /// an index the caller knows is below the length.
    #[inline(always)]
    pub(crate) fn in_capacity(&self, index: usize) -> &T {
        debug_assert!(index < self.len as usize, "in_capacity({index}) past the length {}", self.len);
        &self.items[index]
    }

    /// [`StackVec::in_capacity`], mutably.
    #[inline(always)]
    pub(crate) fn in_capacity_mut(&mut self, index: usize) -> &mut T {
        debug_assert!(index < self.len as usize, "in_capacity_mut({index}) past the length {}", self.len);
        &mut self.items[index]
    }

    /// Removes and returns the last element.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.items[self.len as usize])
    }

    /// Removes every element.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Inserts `item` at `index`, shifting later elements right.
    pub fn insert(&mut self, index: usize, item: T) {
        let len = self.len as usize;
        assert!(index <= len, "insert index {index} out of bounds (len {len})");
        self.push(item);
        self.items[index..=len].rotate_right(1);
    }

    /// Removes and returns the element at `index`, shifting later elements left.
    pub fn remove(&mut self, index: usize) -> T {
        let len = self.len as usize;
        assert!(index < len, "remove index {index} out of bounds (len {len})");
        let item = self.items[index];
        self.items[index..len].rotate_left(1);
        self.len -= 1;
        item
    }

    /// Shortens to at most `len` elements.
    #[inline]
    pub fn truncate(&mut self, len: usize) {
        self.len = (self.len as usize).min(len) as u8;
    }

    /// The number of elements (as the slice's `len`, without building the
    /// slice: its bound check of the length against `N`).
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether it holds no element.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The first element (as the slice's `first`, without building the
    /// slice).
    #[inline(always)]
    pub fn first(&self) -> Option<&T> {
        if self.len > 0 { self.items.first() } else { None }
    }

    /// The element at `index` (as the slice's `get` with an index, without
    /// building the slice; a constant index below `N` costs one check).
    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<&T> {
        if index < self.len as usize { self.items.get(index) } else { None }
    }

    /// The elements from `start` on (as `&self[start..]`, with one test
    /// and no panic: empty past the length).
    #[inline(always)]
    pub(crate) fn tail(&self, start: usize) -> &[T] {
        self.items.get(start..self.len as usize).unwrap_or(&[])
    }

    /// [`StackVec::tail`], mutably.
    #[inline(always)]
    pub(crate) fn tail_mut(&mut self, start: usize) -> &mut [T] {
        self.items.get_mut(start..self.len as usize).unwrap_or(&mut [])
    }

    /// Whether it holds `N` elements.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len as usize == N
    }

    /// The elements mapped by `f`, in order: `f` runs once per element.
    #[inline]
    pub fn map<U: Copy + Default>(&self, mut f: impl FnMut(T) -> U) -> StackVec<U, N> {
        let mut items = [U::default(); N];
        for (out, &item) in items.iter_mut().zip(&self[..]) {
            *out = f(item);
        }
        StackVec { items, len: self.len }
    }

    /// Keeps the elements `keep` accepts, in order.
    #[inline]
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let mut w = 0;
        for r in 0..self.len as usize {
            if keep(&self.items[r]) {
                self.items[w] = self.items[r];
                w += 1;
            }
        }
        self.len = w as u8;
    }
}

impl<T: Copy + Default, const N: usize> Default for StackVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> Deref for StackVec<T, N> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        &self.items[..self.len as usize]
    }
}

impl<T: Copy + Default, const N: usize> DerefMut for StackVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.items[..self.len as usize]
    }
}

/// Compares the elements only, as [`Hash`] hashes them: the unused slots
/// past `len` (whatever was last stored there) don't count.
impl<T: Copy + Default + PartialEq, const N: usize> PartialEq for StackVec<T, N> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl<T: Copy + Default + Eq, const N: usize> Eq for StackVec<T, N> {}

/// Hashes the elements only: the unused slots past `len` (whatever was
/// last stored there) don't count.
impl<T: Copy + Default + Hash, const N: usize> Hash for StackVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self[..].hash(state);
    }
}

impl<T: Copy + Default + fmt::Debug, const N: usize> fmt::Debug for StackVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Copy + Default, const N: usize> FromIterator<T> for StackVec<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = Self::new();
        for x in iter {
            v.push(x);
        }
        v
    }
}

impl<'a, T: Copy + Default, const N: usize> IntoIterator for &'a StackVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::StackVec;

    #[test]
    fn insert_and_remove() {
        let mut v: StackVec<u8, 5> = [1, 2, 4].into_iter().collect();
        v.insert(2, 3);
        v.insert(0, 0);
        assert_eq!(&v[..], &[0, 1, 2, 3, 4]);
        assert_eq!(v.remove(0), 0);
        assert_eq!(v.remove(3), 4);
        assert_eq!(&v[..], &[1, 2, 3]);
        v.truncate(1);
        assert_eq!(&v[..], &[1]);
    }

    /// `map` runs `f` on the elements only, not on the unused slots
    /// (default or left by `pop`).
    #[test]
    fn map_runs_on_the_elements_only() {
        let mut v: StackVec<Option<u8>, 4> = [Some(1), Some(2)].into_iter().collect();
        v.pop();
        let mut calls = 0;
        let m = v.map(|x| {
            calls += 1;
            x.unwrap() * 10
        });
        assert_eq!(calls, 1);
        assert_eq!(&m[..], &[10]);
    }
}
