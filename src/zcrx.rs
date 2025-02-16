// This module contains definitions for io_uring zero-copy reception, as well as a
// Rust wrapper to use it. The raw ABI definitions are ported from this C header file:
// https://github.com/spikeh/linux/blob/8822e8b5bc7d0a18b83a4df74fafb6efc0bbfc2b/include/uapi/linux/
// io_uring.h and the Rust wrapper and documentation comments
// are largely based on the code from the io_uring Rust crate:
// https://github.com/tokio-rs/io-uring/blob/7ad4f7fd06798169f3b0527b9ce1e07e4cb027df/src/lib.rs.

use std::{
    ffi, io,
    mem::{self, ManuallyDrop},
    os::fd::RawFd,
    os::unix::io::AsRawFd,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};

use io_uring::{cqueue, squeue, IoUring};

pub const IORING_OP_RECV_ZC: u8 = 58;

/// Register a netdev hw rx queue for zerocopy.
const IORING_REGISTER_ZCRX_IFQ: u32 = 32;

#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone)]
pub struct io_uring_zcrx_rqe {
    pub off: u64,
    pub len: u32,
    pub __pad: u32,
}

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct io_uring_zcrx_cqe {
    pub off: u64,
    pub __pad: u32,
}

/// The bit from which area id is encoded into offsets.
pub const IORING_ZCRX_AREA_SHIFT: u64 = 48;

pub const IORING_ZCRX_AREA_MASK: u64 = !((1 << IORING_OP_RECV_ZC) - 1);

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

/// Initialise with user provided memory pointed by user_addr.
const IORING_MEM_REGION_TYPE_USER: u32 = 1;

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_area_reg {
    addr: u64,
    len: u64,
    rq_area_token: u64,
    flags: u32,
    __resv1: u32,
    __resv2: [u64; 2],
}

#[inline(always)]
pub(crate) unsafe fn unsync_load(u: *const AtomicU32) -> u32 {
    *u.cast::<u32>()
}

struct Mmap {
    addr: *mut ffi::c_void,
    len: usize,
}

impl Mmap {
    fn new_anon(len: usize) -> io::Result<Self> {
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { addr, len })
    }

    #[inline]
    fn as_mut_ptr(&self) -> *mut ffi::c_void {
        self.addr
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.addr, self.len);
        }
    }
}

unsafe fn io_uring_register(
    fd: RawFd,
    opcode: ffi::c_uint,
    arg: *mut ffi::c_void,
    nr_args: ffi::c_uint,
) -> io::Result<ffi::c_int> {
    let ret = libc::syscall(libc::SYS_io_uring_register, fd, opcode, arg, nr_args) as i32;
    if ret < 0 {
        return Err(io::Error::from_raw_os_error(-ret));
    }
    Ok(ret)
}

unsafe fn io_uring_register_zcrx_ifq(
    fd: RawFd,
    ifq_reg: &mut io_uring_zcrx_ifq_reg,
) -> io::Result<()> {
    io_uring_register(
        fd,
        IORING_REGISTER_ZCRX_IFQ,
        ifq_reg as *mut io_uring_zcrx_ifq_reg as *mut _,
        1,
    )?;
    Ok(())
}

pub struct ZcrxInterfaceQueue {
    area: ManuallyDrop<Mmap>,
    region: ManuallyDrop<Mmap>,
    rq: RefillQueueInner,
}

impl ZcrxInterfaceQueue {
    pub fn new<S: squeue::EntryMarker>(
        ring: &IoUring<S, cqueue::Entry32>,
        if_index: u32,
        rx_queue_index: u32,
        refill_ring_entries: u32,
        area_size: usize,
    ) -> io::Result<Self> {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_mask = !(page_size - 1);
        let refill_ring_size = page_size
            + mem::size_of::<io_uring_zcrx_rqe>() * usize::try_from(refill_ring_entries).unwrap();

        let area = Mmap::new_anon((area_size + page_size - 1) & page_mask)?;
        let area_reg = io_uring_zcrx_area_reg {
            addr: area.as_mut_ptr() as u64,
            len: u64::try_from(area.len()).unwrap(),
            rq_area_token: 0,
            flags: 0,
            __resv1: 0,
            __resv2: [0; 2],
        };

        let region = Mmap::new_anon((refill_ring_size + page_size - 1) & page_mask)?;
        let region_desc = io_uring_region_desc {
            user_addr: region.as_mut_ptr() as u64,
            size: u64::try_from(region.len()).unwrap(),
            flags: IORING_MEM_REGION_TYPE_USER,
            id: 0,
            mmap_offset: 0,
            __resv: [0; 4],
        };
        let region_ptr = region.as_mut_ptr();

        let mut ifq_reg = io_uring_zcrx_ifq_reg {
            if_idx: if_index,
            if_rxq: rx_queue_index,
            rq_entries: refill_ring_entries,
            flags: 0,
            area_ptr: &area_reg as *const _ as u64,
            region_ptr: &region_desc as *const _ as u64,
            offsets: io_uring_zcrx_offsets {
                head: 0,
                tail: 0,
                rqes: 0,
                __resv2: 0,
                __resv: [0; 2],
            },
            __resv: [0; 4],
        };
        unsafe { io_uring_register_zcrx_ifq(ring.as_raw_fd(), &mut ifq_reg)? };

        Ok(Self {
            area: ManuallyDrop::new(area),
            region: ManuallyDrop::new(region),
            rq: unsafe {
                RefillQueueInner::new(
                    region_ptr,
                    ifq_reg.rq_entries,
                    ifq_reg.offsets.head,
                    ifq_reg.offsets.tail,
                    ifq_reg.offsets.rqes,
                )
            },
        })
    }

    /// Release the memory used by the zero-copy interface queue registration without unregistering
    /// it from [`IoUring`].
    ///
    /// # Safety
    ///
    /// Caller must make sure there is no pending zero-copy receive on the [`IoUring`], or the
    /// [`IoUring`] is dropped.
    pub unsafe fn drop(mut self) {
        ManuallyDrop::drop(&mut self.area);
        ManuallyDrop::drop(&mut self.region);
    }

    /// Get the refill queue. This is used to recycle buffers that were
    /// used for zero-copy receive operations.
    #[inline]
    pub fn refill(&mut self) -> RefillQueue<'_> {
        self.rq.borrow()
    }

    /// Get the refill queue from a shared reference.
    ///
    /// # Safety
    ///
    /// No other [`RefillQueue`]s may exist when calling this function.
    #[inline]
    pub unsafe fn refill_shared(&self) -> RefillQueue<'_> {
        self.rq.borrow_shared()
    }
}

struct RefillQueueInner {
    head: *const AtomicU32,
    tail: *const AtomicU32,
    ring_entries: u32,
    ring_mask: u32,
    rqes: *mut io_uring_zcrx_rqe,
}

impl RefillQueueInner {
    unsafe fn new(
        region: *mut ffi::c_void,
        ring_entries: u32,
        head_offset: u32,
        tail_offset: u32,
        rqes_offset: u32,
    ) -> RefillQueueInner {
        debug_assert!(ring_entries.is_power_of_two());
        let ring_mask = ring_entries - 1;

        Self {
            head: region.offset(head_offset as isize).cast(),
            tail: region.offset(tail_offset as isize).cast(),
            ring_entries,
            ring_mask,
            rqes: region.offset(rqes_offset as isize).cast(),
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

pub struct PushError;

// The code for the refill queue wrapper is pretty much a copy of
// the code for the `io_uring` crate's submission queue wrapper
// (https://github.com/tokio-rs/io-uring/blob/7ad4f7fd06798169f3b0527b9ce1e07e4cb027df/src/squeue.rs),
// where mentions of submission queue entries have been replaced with refill queue entries.
pub struct RefillQueue<'a> {
    head: u32,
    tail: u32,
    queue: &'a RefillQueueInner,
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

// The io_uring crate does not know know about the IORING_OP_RECV_ZC operation and it has no way
// to create a SQE with a custom opcode, so redefine the bits of the SQE structure needed for
// IORING_OP_RECV_ZC operations.
#[repr(C)]
#[allow(non_camel_case_types)]
pub(crate) struct io_uring_sqe {
    pub(crate) opcode: u8,
    pub(crate) flags: u8,
    pub(crate) ioprio: u16,
    pub(crate) fd: i32,
    pub(crate) off: u64,
    pub(crate) addr: u64,
    pub(crate) len: u32,
    pub(crate) rw_flags: ffi::c_int,
    pub(crate) user_data: u64,
    pub(crate) buf_group: u16,
    pub(crate) personality: u16,
    pub(crate) file_index: u32,
    pub(crate) addr3: u64,
    pub(crate) __pad2: [u64; 1],
}

pub const IORING_RECV_MULTISHOT: u16 = 0x2;
