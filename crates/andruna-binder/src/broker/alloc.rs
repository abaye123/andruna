//! The allocator of one process's receive buffer: first fit over a free
//! list, 8-byte granules, adjacent free ranges merged.

use std::collections::BTreeMap;

/// Free ranges of a buffer of `len` bytes.
#[derive(Debug, Default)]
pub struct Allocator {
    len: usize,
    /// Start to length.
    free: BTreeMap<usize, usize>,
}

pub const ALIGN: usize = 8;

pub fn align8(n: usize) -> Option<usize> {
    n.checked_add(ALIGN - 1).map(|v| v & !(ALIGN - 1))
}

impl Allocator {
    pub fn new(len: usize) -> Self {
        let len = len & !(ALIGN - 1);
        let mut free = BTreeMap::new();
        if len > 0 {
            free.insert(0, len);
        }
        Self { len, free }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Allocates `size` bytes (rounded up to 8, at least 8); the offset.
    pub fn alloc(&mut self, size: usize) -> Option<usize> {
        let size = align8(size.max(ALIGN))?;
        let (&start, &len) = self.free.iter().find(|(_, l)| **l >= size)?;
        self.free.remove(&start);
        if len > size {
            self.free.insert(start + size, len - size);
        }
        Some(start)
    }

    /// Frees the allocation at `start` of `size` bytes (as passed to
    /// [`Self::alloc`]).
    pub fn free(&mut self, start: usize, size: usize) {
        let Some(mut size) = align8(size.max(ALIGN)) else {
            return;
        };
        let mut start = start;
        if let Some((&next, &nlen)) = self.free.range(start + size..).next()
            && next == start + size
        {
            self.free.remove(&next);
            size += nlen;
        }
        if let Some((&prev, &plen)) = self.free.range(..start).next_back()
            && prev + plen == start
        {
            self.free.remove(&prev);
            start = prev;
            size += plen;
        }
        self.free.insert(start, size);
    }

    /// Bytes free in all.
    pub fn free_bytes(&self) -> usize {
        self.free.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fit_and_merge() {
        let mut a = Allocator::new(64);
        let x = a.alloc(10).unwrap();
        let y = a.alloc(8).unwrap();
        let z = a.alloc(30).unwrap();
        assert_eq!((x, y, z), (0, 16, 24));
        assert_eq!(a.alloc(17), None);
        assert_eq!(a.alloc(0), Some(56));
        a.free(y, 8);
        assert_eq!(a.alloc(9), None);
        a.free(x, 10);
        assert_eq!(a.alloc(24), Some(0));
        a.free(0, 24);
        a.free(z, 30);
        a.free(56, 0);
        assert_eq!(a.free_bytes(), 64);
        assert_eq!(a.alloc(64), Some(0));
    }
}
