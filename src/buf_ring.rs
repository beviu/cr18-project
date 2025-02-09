use std::{
    io, mem,
    sync::atomic::{AtomicU16, Ordering},
};

use io_uring::{types::BufRingEntry, Submitter};
use memmap2::MmapMut;

/// The memory mapping for a io_uring buffer ring.
pub struct BufRingMmap {
    /// The number of entries in the buffer ring.
    ///
    /// # Safety
    ///
    /// Must be positive.
    entry_count: u16,

    mmap: memmap2::MmapMut,
}

impl BufRingMmap {
    pub fn new(entry_count: u16) -> io::Result<Self> {
        if entry_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer ring must contain at least one entry",
            ));
        }
        let size = mem::size_of::<BufRingEntry>() * usize::from(entry_count);
        let mmap = MmapMut::map_anon(size)?;
        Ok(Self { entry_count, mmap })
    }

    pub fn entry_count(&self) -> u16 {
        self.entry_count
    }

    pub fn as_ptr(&self) -> *const BufRingEntry {
        self.mmap.as_ptr() as *mut _
    }

    pub fn as_mut_ptr(&mut self) -> *mut BufRingEntry {
        self.mmap.as_mut_ptr() as *mut _
    }

    pub fn tail(&self) -> &AtomicU16 {
        unsafe { &*(BufRingEntry::tail(self.as_ptr()) as *const AtomicU16) }
    }

    pub fn mask(&self) -> u16 {
        self.entry_count - 1
    }
}

/// An io_uring buffer ring registration.
pub struct BufRing<'a> {
    submitter: &'a Submitter<'a>,
    bgid: u16,
    mmap: BufRingMmap,
    tail: u16,
}

impl<'a> BufRing<'a> {
    pub fn register(
        submitter: &'a Submitter<'a>,
        bgid: u16,
        mut mmap: BufRingMmap,
    ) -> io::Result<Self> {
        let tail = mmap.tail().load(Ordering::Relaxed);
        let ring_addr = mmap.as_mut_ptr();
        unsafe {
            submitter.register_buf_ring(ring_addr as u64, mmap.entry_count, bgid)?;
        }
        Ok(Self {
            submitter,
            bgid,
            mmap,
            tail,
        })
    }

    pub fn entry_count(&self) -> u16 {
        self.mmap.entry_count()
    }

    pub fn sync(&self) {
        self.mmap.tail().store(self.tail, Ordering::Release);
    }

    pub unsafe fn add_buffer(&mut self, buf: *mut [u8], id: u16) {
        let index = usize::from(self.tail & self.mmap.mask());
        let entry = &mut *self.mmap.as_mut_ptr().add(index);
        entry.set_addr(buf as *mut u8 as u64);
        entry.set_len(u32::try_from(buf.len()).expect("buffer length is too large"));
        entry.set_bid(id);
        self.tail += 1;
    }
}

impl<'a> Drop for BufRing<'a> {
    fn drop(&mut self) {
        self.submitter
            .unregister_buf_ring(self.bgid)
            .expect("failed to unregister buffer ring");
    }
}
