use std::sync::atomic::{AtomicU32, Ordering};

const IORING_OP_RECV_ZC: u8 = 58;

/// Register a netdev hw rx queue for zerocopy.
const IORING_REGISTER_ZCRX_IFQ: u32 = 32;

#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone)]
struct io_uring_zcrx_rqe {
    off: u64,
    len: u32,
    __pad: u32,
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_cqe {
    off: u64,
    __pad: u32,
}

/// The bit from which area id is encoded into offsets.
const IORING_ZCRX_AREA_SHIFT: u64 = 48;

const IORING_ZCRX_AREA_MASK: u64 = !((1 << IORING_OP_RECV_ZC) - 1);

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_offsets {
    head: u32,
    tail: u32,
    rqes: u32,
    __resv2: u32,
    __resv: [u64; 2],
}

/// Argument for IORING_REGISTER_ZCRX_IFQ.
#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_ifq_reg {
    if_idx: u32,
    if_rxq: u32,
    rq_entries: u32,
    flags: u32,

    area_ptr: u64,
    region_ptr: u64,

    offsets: io_uring_zcrx_offsets,
    __resv: [u64; 4],
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_region_desc {
    user_addr: u64,
    size: u64,
    flags: u32,
    id: u32,
    mmap_offset: u64,
    __resv: [u64; 4],
}

#[inline(always)]
pub(crate) unsafe fn unsync_load(u: *const AtomicU32) -> u32 {
    *u.cast::<u32>()
}

struct Inner {
    head: *const AtomicU32,
    tail: *const AtomicU32,
    ring_entries: u32,
    ring_mask: u32,
    rqes: *mut io_uring_zcrx_rqe,
}

impl Inner {
    unsafe fn new(ifq_reg: &io_uring_zcrx_ifq_reg) -> Inner {
        let region_desc = &*(ifq_reg.region_ptr as *const io_uring_region_desc);
        let region_ptr = region_desc.user_addr as *const u8;

        debug_assert!(ifq_reg.rq_entries.is_power_of_two());
        let ring_mask = ifq_reg.rq_entries - 1;

        Self {
            head: region_ptr.offset(ifq_reg.offsets.head as isize).cast(),
            tail: region_ptr.offset(ifq_reg.offsets.tail as isize).cast(),
            ring_entries: ifq_reg.rq_entries,
            ring_mask,
            rqes: region_ptr
                .offset(ifq_reg.offsets.rqes as isize)
                .cast_mut()
                .cast(),
        }
    }

    #[inline]
    pub(crate) unsafe fn borrow_shared(&self) -> RefillQueue<'_> {
        RefillQueue {
            head: (*self.head).load(Ordering::Acquire),
            tail: unsync_load(self.tail),
            queue: self,
        }
    }

    #[inline]
    pub(crate) fn borrow(&mut self) -> RefillQueue<'_> {
        unsafe { self.borrow_shared() }
    }
}

struct PushError;

// The code for the refill queue wrapper is pretty much a copy of
// the code for the `io_uring` crate's submission queue wrapper
// (https://github.com/tokio-rs/io-uring/blob/7ad4f7fd06798169f3b0527b9ce1e07e4cb027df/src/squeue.rs),
// where mentions of submission queue entries have been replaced with refill queue entries.
struct RefillQueue<'a> {
    head: u32,
    tail: u32,
    queue: &'a Inner,
}

impl<'a> RefillQueue<'a> {
    pub fn sync(&mut self) {
        unsafe { &*self.queue.tail }.store(self.tail, Ordering::Release);
        unsafe { &*self.queue.head }.load(Ordering::Acquire);
    }

    /// Get the total number of entries in the refill queue ring buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.queue.ring_entries as usize
    }

    /// Get the number of refill queue events in the ring buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.tail.wrapping_sub(self.head) as usize
    }

    /// Returns `true` if the refill queue ring buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` if the refill queue ring buffer has reached capacity, and no more buffers can
    /// be added before the kernel consumes some.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    /// Attempts to push an entry into the queue.
    /// If the queue is full, an error is returned.
    ///
    /// # Safety
    ///
    /// Developers must ensure that parameters of the entry are valid and will be valid for the
    /// entire duration of the zero-copy receive operations, otherwise it may cause memory problems.
    #[inline]
    pub unsafe fn push(&mut self, entry: &io_uring_zcrx_rqe) -> Result<(), PushError> {
        if !self.is_full() {
            self.push_unchecked(entry);
            Ok(())
        } else {
            Err(PushError)
        }
    }

    /// Attempts to push several entries into the queue.
    /// If the queue does not have space for all of the entries, an error is returned.
    ///
    /// # Safety
    ///
    /// Developers must ensure that parameters of all the entries (such as buffer) are valid and
    /// will be valid for the entire duration of the zero-copy receive operations, otherwise it may
    /// cause memory problems.
    #[inline]
    pub unsafe fn push_multiple(&mut self, entries: &[io_uring_zcrx_rqe]) -> Result<(), PushError> {
        if self.capacity() - self.len() < entries.len() {
            return Err(PushError);
        }

        for entry in entries {
            self.push_unchecked(entry);
        }

        Ok(())
    }

    #[inline]
    unsafe fn push_unchecked(&mut self, entry: &io_uring_zcrx_rqe) {
        *self
            .queue
            .rqes
            .add((self.tail & self.queue.ring_mask) as usize) = entry.clone();
        self.tail = self.tail.wrapping_add(1);
    }
}

impl<'a> Drop for RefillQueue<'a> {
    fn drop(&mut self) {
        unsafe { &*self.queue.tail }.store(self.tail, Ordering::Release);
    }
}
