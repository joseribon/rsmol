use std::fmt;

/// A set of atom indices in one structure, stored as a bitset.
#[derive(Clone, PartialEq, Eq)]
pub struct AtomSet {
    words: Vec<u64>,
    len: usize,
}

impl AtomSet {
    pub fn empty(len: usize) -> Self {
        AtomSet { words: vec![0; len.div_ceil(64)], len }
    }

    pub fn full(len: usize) -> Self {
        let mut s = AtomSet { words: vec![u64::MAX; len.div_ceil(64)], len };
        s.clear_tail();
        s
    }

    pub fn from_fn(len: usize, mut f: impl FnMut(usize) -> bool) -> Self {
        let mut s = AtomSet::empty(len);
        for i in 0..len {
            if f(i) {
                s.insert(i);
            }
        }
        s
    }

    pub fn from_indices(len: usize, indices: impl IntoIterator<Item = usize>) -> Self {
        let mut s = AtomSet::empty(len);
        for i in indices {
            s.insert(i);
        }
        s
    }

    /// Number of atoms in the structure (the universe), not the number selected.
    pub fn universe(&self) -> usize {
        self.len
    }

    pub fn contains(&self, i: usize) -> bool {
        i < self.len && self.words[i / 64] & (1 << (i % 64)) != 0
    }

    pub fn insert(&mut self, i: usize) {
        assert!(i < self.len, "atom {i} out of range for {} atoms", self.len);
        self.words[i / 64] |= 1 << (i % 64);
    }

    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &word)| {
            let mut w = word;
            std::iter::from_fn(move || {
                (w != 0).then(|| {
                    let bit = w.trailing_zeros() as usize;
                    w &= w - 1;
                    wi * 64 + bit
                })
            })
        })
    }

    pub fn first(&self) -> Option<usize> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<usize> {
        let (wi, &w) = self.words.iter().enumerate().rev().find(|(_, w)| **w != 0)?;
        Some(wi * 64 + 63 - w.leading_zeros() as usize)
    }

    pub fn and(mut self, other: &AtomSet) -> Self {
        self.zip_with(other, |a, b| a & b);
        self
    }

    pub fn or(mut self, other: &AtomSet) -> Self {
        self.zip_with(other, |a, b| a | b);
        self
    }

    pub fn and_not(mut self, other: &AtomSet) -> Self {
        self.zip_with(other, |a, b| a & !b);
        self
    }

    pub fn complement(mut self) -> Self {
        for w in &mut self.words {
            *w = !*w;
        }
        self.clear_tail();
        self
    }

    fn zip_with(&mut self, other: &AtomSet, f: impl Fn(u64, u64) -> u64) {
        assert_eq!(self.len, other.len, "atom sets from different structures");
        for (a, &b) in self.words.iter_mut().zip(&other.words) {
            *a = f(*a, b);
        }
    }

    /// Keeps bits past `len` zero so counts and comparisons stay exact.
    fn clear_tail(&mut self) {
        if !self.len.is_multiple_of(64)
            && let Some(last) = self.words.last_mut()
        {
            *last &= (1u64 << (self.len % 64)) - 1;
        }
    }
}

impl fmt::Debug for AtomSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AtomSet({} of {}: ", self.count(), self.len)?;
        f.debug_list().entries(self.iter().take(32)).finish()?;
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations_respect_the_universe() {
        let a = AtomSet::from_indices(130, [0, 63, 64, 129]);
        let b = AtomSet::from_indices(130, [63, 100]);
        assert_eq!(a.clone().and(&b).iter().collect::<Vec<_>>(), [63]);
        assert_eq!(a.clone().or(&b).count(), 5);
        assert_eq!(a.clone().and_not(&b).iter().collect::<Vec<_>>(), [0, 64, 129]);
        assert_eq!(a.clone().complement().count(), 126);
        assert_eq!(AtomSet::full(130).count(), 130);
        assert_eq!((a.first(), a.last()), (Some(0), Some(129)));
        assert_eq!(AtomSet::empty(5).last(), None);
    }
}
