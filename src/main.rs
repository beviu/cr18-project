use std::{
    ffi::{c_uint, c_void},
    fs::File,
    io,
    mem::{self, MaybeUninit},
    net::{SocketAddr, UdpSocket},
    num::NonZeroUsize,
    os::fd::{AsRawFd, FromRawFd},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
    thread,
};

use clap::Parser;
use io_uring::{
    cqueue, squeue,
    types::{CancelBuilder, TimeoutFlags, Timespec},
    IoUring,
};
use io_uring_buf_ring::IoUringBufRing;

mod zcrx;

#[derive(clap::Parser)]
struct Args {
    /// Use fixed files instead of file descriptors.
    #[clap(short, long)]
    fixed_files: bool,

    /// Use fixed buffers for sending.
    #[clap(short = 'F', long)]
    fixed_buffers: bool,

    /// Use zero-copy sends.
    #[clap(short, long)]
    zero_copy: bool,

    /// Use a buffer ring.
    #[clap(short, long)]
    buf_ring: bool,

    /// Number of threads to use. By default, the number of logical cores is used.
    #[clap(short, long)]
    threads: Option<NonZeroUsize>,

    /// Run as a server instead of a client.
    #[clap(short, long)]
    server: bool,

    /// The address to bind the socket to.
    #[clap(short = 'B', long, default_value = "[::]:0")]
    bind: String,

    /// The address to send datagrams to.
    #[clap(short, long, default_value = "[::]:12000")]
    dest: SocketAddr,

    /// Use io_uring's `IORING_SETUP_SINGLE_ISSUER` option.
    #[clap(long)]
    single_issuer: bool,

    /// Use io_uring's `IORING_SETUP_COOP_TASKRUN` option.
    #[clap(long)]
    coop_taskrun: bool,
}

#[repr(C)]
union sockaddr_in {
    in_: libc::sockaddr_in,
    in6: libc::sockaddr_in6,
}

fn send_datagrams(
    socket: &UdpSocket,
    dest: &SocketAddr,
    fixed_files: bool,
    fixed_buffers: bool,
    zero_copy: bool,
    single_issuer: bool,
    coop_taskrun: bool,
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

    const DATAGRAM: [u8; 16] = [1; 16];

    if fixed_buffers {
        let iovec = libc::iovec {
            iov_base: DATAGRAM.as_ptr() as *mut _,
            iov_len: DATAGRAM.len(),
        };
        unsafe {
            ring.submitter()
                .register_buffers(&[iovec])
                .expect("failed to register send buffer");
        }
    }

    const USER_DATA_STOP: u64 = 0;
    const USER_DATA_SEND: u64 = 1;

    {
        const FUTEX2_SIZE_U32: u32 = 0x2;
        let wait = io_uring::opcode::FutexWait::new(
            stop.as_ptr() as *const _,
            0,
            libc::FUTEX_BITSET_MATCH_ANY as c_uint as u64,
            FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as u32,
        )
        .build()
        .user_data(USER_DATA_STOP);

        let mut submission = ring.submission();
        unsafe {
            submission.push(&wait).unwrap();
        }
    }

    let (addr, addr_len) = match dest {
        SocketAddr::V4(v4) => {
            let in_ = libc::sockaddr_in {
                sin_family: u16::try_from(libc::AF_INET).unwrap(),
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: v4.ip().to_bits().to_be(),
                },
                sin_zero: [0; 8],
            };
            let addr_len = u32::try_from(mem::size_of_val(&in_)).unwrap();
            (sockaddr_in { in_ }, addr_len)
        }
        SocketAddr::V6(v6) => {
            let in6 = libc::sockaddr_in6 {
                sin6_family: u16::try_from(libc::AF_INET6).unwrap(),
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            let addr_len = u32::try_from(mem::size_of_val(&in6)).unwrap();
            (sockaddr_in { in6 }, addr_len)
        }
    };

    let mut datagram_count = 0;

    'main_loop: while stop.load(Ordering::Relaxed) == 0 {
        {
            let mut submission = ring.submission();
            while !submission.is_full() {
                let datagram_len = u32::try_from(DATAGRAM.len()).unwrap();

                let entry = if zero_copy {
                    let mut send = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::SendZc::new(fixed, DATAGRAM.as_ptr(), datagram_len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::SendZc::new(fd, DATAGRAM.as_ptr(), datagram_len)
                    };

                    if fixed_buffers {
                        send = send.buf_index(Some(0));
                    }

                    send.dest_addr(&addr as *const sockaddr_in as *const _)
                        .dest_addr_len(addr_len)
                        .build()
                } else {
                    let send = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::Send::new(fixed, DATAGRAM.as_ptr(), datagram_len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::Send::new(fd, DATAGRAM.as_ptr(), datagram_len)
                    };

                    if fixed_buffers {
                        panic!("fixed buffers is only supported when zero-copy is enabled");
                    }

                    send.dest_addr(&addr as *const sockaddr_in as *const _)
                        .dest_addr_len(addr_len)
                        .build()
                };
                let entry = entry.user_data(USER_DATA_SEND);

                unsafe {
                    submission.push(&entry).unwrap();
                }
            }
        }

        for entry in ring.completion() {
            let user_data = entry.user_data();
            match user_data {
                USER_DATA_SEND => {
                    if !cqueue::notif(entry.flags()) {
                        if entry.result() < 0 {
                            eprintln!("send: {}", entry.result());
                        } else {
                            datagram_count += 1;
                        }
                    }
                }
                USER_DATA_STOP => {
                    if entry.result() < 0 {
                        eprintln!("futex_wait: {}", entry.result());
                        break 'main_loop;
                    }
                }
                _ => panic!("unexpected user data {user_data} in CQE"),
            }
        }

        ring.submitter().submit_and_wait(1).unwrap();
    }

    ring.submitter()
        .register_sync_cancel(None, CancelBuilder::any())
        .expect("failed to cancel pending requests");

    datagram_count
}

fn receive_datagrams(
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
            iov_base: buf.as_mut_ptr() as *mut c_void,
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
            libc::FUTEX_BITSET_MATCH_ANY as c_uint as u64,
            FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as c_uint,
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

fn mask_sigint() -> io::Result<()> {
    let mut mask: MaybeUninit<libc::sigset_t> = MaybeUninit::uninit();
    unsafe {
        libc::sigemptyset(mask.as_mut_ptr());
    }
    unsafe {
        libc::sigaddset(mask.as_mut_ptr(), libc::SIGINT);
    }

    let ret = unsafe { libc::sigprocmask(libc::SIG_BLOCK, mask.as_ptr(), ptr::null_mut()) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn signalfd_full(flags: i32) -> io::Result<File> {
    let mut mask: MaybeUninit<libc::sigset_t> = MaybeUninit::uninit();
    unsafe {
        libc::sigfillset(mask.as_mut_ptr());
    }

    let fd = unsafe { libc::signalfd(-1, mask.as_ptr(), flags) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }

    let file = unsafe { File::from_raw_fd(fd) };
    Ok(file)
}

fn futex_wake_all(futex: &AtomicU32) {
    let op = libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG;
    unsafe {
        libc::syscall(libc::SYS_futex, futex.as_ptr(), op, i32::MAX);
    }
}

fn main() {
    let args = Args::parse();

    let socket = UdpSocket::bind(args.bind).expect("failed to bind socket");

    let thread_count = args
        .threads
        .unwrap_or_else(|| thread::available_parallelism().unwrap());

    mask_sigint().expect("failed to mask SIGINT");

    let signalfd = signalfd_full(libc::SFD_CLOEXEC).expect("failed to create signalfd");

    let mut main_ring: IoUring<squeue::Entry, cqueue::Entry> = IoUring::builder()
        .setup_single_issuer()
        .setup_coop_taskrun()
        .build(8)
        .expect("failed to create main io_uring instance");

    let mut siginfo: MaybeUninit<libc::signalfd_siginfo> = MaybeUninit::uninit();

    {
        let timespec = Timespec::new().sec(1);

        {
            let mut submission = main_ring.submission();

            if !args.server {
                let timeout = io_uring::opcode::Timeout::new(&timespec)
                    .flags(TimeoutFlags::BOOTTIME)
                    .build();
                unsafe {
                    submission.push(&timeout).unwrap();
                }
            }

            let fd = io_uring::types::Fd(signalfd.as_raw_fd());
            let read = io_uring::opcode::Read::new(
                fd,
                siginfo.as_mut_ptr() as *mut _,
                u32::try_from(mem::size_of_val(&siginfo)).unwrap(),
            )
            .build();
            unsafe {
                submission.push(&read).unwrap();
            }
        }

        main_ring
            .submitter()
            .submit()
            .expect("failed to submit SQEs on main io_uring instance");
    }

    let stop = AtomicU32::new(0);

    let datagram_count: u64 = thread::scope(|s| {
        let mut threads = Vec::new();

        for _ in 0..thread_count.get() {
            threads.push(s.spawn(|| {
                if args.server {
                    receive_datagrams(
                        &socket,
                        args.fixed_files,
                        args.fixed_buffers,
                        args.single_issuer,
                        args.coop_taskrun,
                        args.buf_ring,
                        &stop,
                    )
                } else {
                    send_datagrams(
                        &socket,
                        &args.dest,
                        args.fixed_files,
                        args.fixed_buffers,
                        args.zero_copy,
                        args.single_issuer,
                        args.coop_taskrun,
                        &stop,
                    )
                }
            }));
        }

        loop {
            main_ring
                .submitter()
                .submit_and_wait(1)
                .expect("failed to wait on main io_uring instance");
            if main_ring.completion().next().is_some() {
                break;
            }
        }

        stop.store(1, Ordering::Relaxed);
        futex_wake_all(&stop);

        threads
            .into_iter()
            .map(|t| t.join().expect("thread panicked"))
            .sum()
    });

    println!("basic: {}", datagram_count);
}
