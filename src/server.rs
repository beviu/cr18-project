use std::{
    ffi, mem,
    net::UdpSocket,
    ops::SubAssign,
    os::fd::AsRawFd,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};

use io_uring::{cqueue, squeue, types::CancelBuilder, IoUring};
use io_uring_buf_ring::IoUringBufRing;

use crate::zcrx::{io_uring_sqe, ZcrxInterfaceQueue, IORING_OP_RECV_ZC, IORING_RECV_MULTISHOT};

pub fn receive_datagrams(
    socket: &UdpSocket,
    fixed_files: bool,
    fixed_buffers: bool,
    single_issuer: bool,
    coop_taskrun: bool,
    use_buf_ring: bool,
    stop: &AtomicU32,
) -> u64 {
    let mut builder: io_uring::Builder<squeue::Entry, cqueue::Entry> = IoUring::builder();

    if single_issuer {
        builder.setup_single_issuer();
    }

    if coop_taskrun {
        builder.setup_coop_taskrun();
    }

    let mut ring = builder
        .build(8)
        .expect("failed to create thread io_uring instance");

    if fixed_files {
        ring.submitter()
            .register_files(&[socket.as_raw_fd()])
            .unwrap();
    }

    let buf_ring = IoUringBufRing::new(&ring, 8, 0, 16).expect("failed to create buf_ring");
    let mut buf_ring_available_count = 8;

    const BUF_SIZE: usize = 16;

    let mut iovecs = Vec::new();
    let mut bufs = Vec::new();
    let mut available_buf_indices = Vec::new();

    for buf_index in 0..8 {
        let mut buf = Box::new([1u8; BUF_SIZE]);
        iovecs.push(libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        });
        bufs.push(buf);
        available_buf_indices.push(buf_index);
    }

    if fixed_buffers {
        unsafe {
            ring.submitter().register_buffers(&iovecs).unwrap();
        }
    }

    const USER_DATA_STOP: u64 = 0;
    const USER_DATA_RECV_FIRST: u64 = 1;

    {
        const FUTEX2_SIZE_U32: u32 = 0x2;
        let wait = io_uring::opcode::FutexWait::new(
            stop.as_ptr() as *const _,
            0,
            libc::FUTEX_BITSET_MATCH_ANY as ffi::c_uint as u64,
            FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as ffi::c_uint,
        )
        .build()
        .user_data(USER_DATA_STOP);

        let mut submission = ring.submission();
        unsafe {
            submission.push(&wait).unwrap();
        }
    }

    let mut datagram_count = 0;

    'main_loop: while stop.load(Ordering::Relaxed) == 0 {
        if !available_buf_indices.is_empty() {
            let mut submission = ring.submission();
            while !submission.is_full() && !available_buf_indices.is_empty() {
                let len = u32::try_from(BUF_SIZE).unwrap();
                let entry = if use_buf_ring {
                    if buf_ring_available_count == 0 {
                        break;
                    }
                    buf_ring_available_count -= 1;

                    let recv = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::Recv::new(fixed, ptr::null_mut(), len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::Recv::new(fd, ptr::null_mut(), len)
                    };

                    recv.buf_group(buf_ring.buffer_group())
                        .build()
                        .user_data(USER_DATA_RECV_FIRST)
                        .flags(squeue::Flags::BUFFER_SELECT)
                } else {
                    let Some(buf_index) = available_buf_indices.pop() else {
                        break;
                    };

                    let recv = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::Recv::new(fixed, bufs[buf_index].as_mut_ptr(), len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::Recv::new(fd, bufs[buf_index].as_mut_ptr(), len)
                    };

                    recv.build()
                        .user_data(USER_DATA_RECV_FIRST + u64::try_from(buf_index).unwrap())
                };
                unsafe {
                    submission.push(&entry).unwrap();
                }
            }
        }

        for entry in ring.completion() {
            match entry.user_data() {
                USER_DATA_STOP => {
                    if entry.result() < 0 {
                        eprintln!("futex_wait: {}", entry.result());
                        break 'main_loop;
                    }
                }
                user_data => {
                    if use_buf_ring {
                        if user_data != USER_DATA_RECV_FIRST {
                            panic!("unexpected user data {user_data} in CQE");
                        }
                        let buf_id = cqueue::buffer_select(entry.flags())
                            .expect("missing buffer ID in recv CQE");
                        match usize::try_from(entry.result()) {
                            Ok(available_len) => {
                                datagram_count += 1;
                                mem::drop(unsafe { buf_ring.get_buf(buf_id, available_len) });
                                buf_ring_available_count += 1;
                            }
                            Err(_) => eprintln!("recv: {}", entry.result()),
                        }
                    } else {
                        if let Some(buf_index) = user_data.checked_sub(USER_DATA_RECV_FIRST) {
                            if entry.result() < 0 {
                                eprintln!("recv: {}", entry.result());
                            } else {
                                datagram_count += 1;
                            }
                            available_buf_indices.push(usize::try_from(buf_index).unwrap());
                        } else {
                            panic!("unexpected user data {user_data} in CQE");
                        }
                    }
                }
            }
        }

        ring.submitter().submit_and_wait(1).unwrap();
    }

    ring.submitter()
        .register_sync_cancel(None, CancelBuilder::any())
        .expect("failed to cancel pending requests");

    if let Err(err) = unsafe { buf_ring.release(&ring) } {
        eprintln!("failed to release buf_ring: {err}");
    }

    datagram_count
}

pub fn receive_datagrams_zc(
    socket: &UdpSocket,
    if_index: u32,
    rx_queue_index: u32,
    stop: &AtomicU32,
) -> u64 {
    let mut ring = IoUring::builder()
        .setup_single_issuer()
        .setup_coop_taskrun()
        .build(8)
        .expect("failed to create thread io_uring instance");

    ring.submitter()
        .register_files(&[socket.as_raw_fd()])
        .unwrap();

    let ifq = ZcrxInterfaceQueue::new(&ring, if_index, rx_queue_index, 32, 8192)
        .expect("failed to register interface queue for zero-copy receive");

    const USER_DATA_STOP: u64 = 0;
    const USER_DATA_RECV: u64 = 1;

    const FUTEX2_SIZE_U32: u32 = 0x2;
    let wait = io_uring::opcode::FutexWait::new(
        stop.as_ptr() as *const _,
        0,
        libc::FUTEX_BITSET_MATCH_ANY as ffi::c_uint as u64,
        FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as ffi::c_uint,
    )
    .build()
    .user_data(USER_DATA_STOP);

    const IOSQE_FIXED_FILE: u8 = 0x1;
    let recv = io_uring_sqe {
        opcode: IORING_OP_RECV_ZC,
        flags: IOSQE_FIXED_FILE,
        ioprio: IORING_RECV_MULTISHOT,
        fd: 0,
        off: 0,
        addr: 0,
        len: 16,
        rw_flags: 0,
        user_data: USER_DATA_RECV,
        buf_group: 0,
        personality: 0,
        file_index: 0,
        addr3: 0,
        __pad2: [0; 1],
    };
    let recv: squeue::Entry = unsafe { mem::transmute(recv) };

    unsafe { ring.submission().push_multiple(&[wait, recv]).unwrap(); }
    
    let mut datagram_count = 0;

    'main_loop: while stop.load(Ordering::Relaxed) == 0 {
        for entry in ring.completion() {
            match entry.user_data() {
                USER_DATA_STOP => {
                    if entry.result() < 0 {
                        eprintln!("futex_wait: {}", entry.result());
                        break 'main_loop;
                    }
                }
                USER_DATA_RECV => {
                    println!("receive!");
                }
                user_data => panic!("unexpected user data {user_data} in CQE"),
            }
        }

        ring.submitter().submit_and_wait(1).unwrap();
    }

    ring.submitter()
        .register_sync_cancel(None, CancelBuilder::any())
        .expect("failed to cancel pending requests");

    unsafe { ifq.drop(); }

    datagram_count
}
