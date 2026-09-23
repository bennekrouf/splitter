//! Single-producer single-consumer f32 ring buffer. The audio callback is the only consumer
//! and never locks or allocates.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Ring {
    buf: Box<[UnsafeCell<f32>]>,
    /// Total samples ever read / written; indices are taken modulo capacity.
    read: AtomicUsize,
    write: AtomicUsize,
}

// SAFETY: one producer writes only the free region, one consumer reads only the filled region;
// the Acquire/Release pairs on the indices publish the data between them.
unsafe impl Sync for Ring {}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: (0..capacity).map(|_| UnsafeCell::new(0.0)).collect(),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }
    }

    fn cap(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.write.load(Ordering::Acquire).wrapping_sub(self.read.load(Ordering::Acquire))
    }

    pub fn free(&self) -> usize {
        self.cap() - self.len()
    }

    /// Producer: push as many whole `align`-sized frames as fit. Returns samples pushed.
    pub fn push(&self, data: &[f32], align: usize) -> usize {
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        let free = self.cap() - w.wrapping_sub(r);
        let n = data.len().min(free) / align * align;
        for (i, &s) in data[..n].iter().enumerate() {
            unsafe { *self.buf[(w + i) % self.cap()].get() = s };
        }
        self.write.store(w.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer: pop into `out`. Returns samples popped.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        let n = out.len().min(w.wrapping_sub(r));
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = unsafe { *self.buf[(r + i) % self.cap()].get() };
        }
        self.read.store(r.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer: drop everything currently buffered.
    pub fn clear(&self) {
        self.read.store(self.write.load(Ordering::Acquire), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_aligns() {
        let ring = Ring::new(8);
        assert_eq!(ring.push(&[1.0; 7], 2), 6);
        let mut out = [0.0; 4];
        assert_eq!(ring.pop(&mut out), 4);
        assert_eq!(ring.push(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0], 2), 6);
        let mut out = [0.0; 8];
        assert_eq!(ring.pop(&mut out), 8);
        assert_eq!(&out[..4], &[1.0, 1.0, 2.0, 3.0]);
        assert_eq!(ring.len(), 0);
    }
}
