use std::{
    ffi::{c_uint, c_void},
    fs::File,
    io,
    mem::{self, MaybeUninit},
    net::UdpSocket,
    num::NonZeroUsize,
    os::fd::{AsRawFd, FromRawFd},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
    thread,
};

use buf_ring::{BufRing, BufRingMmap};
use clap::Parser;
use io_uring::types::{TimeoutFlags, Timespec};

mod buf_ring;

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
}

fn send_datagrams(
    socket: &UdpSocket,
    fixed_files: bool,
    fixed_buffers: bool,
    zero_copy: bool,
    stop: &AtomicU32,
) -> u64 {
    let mut ring = io_uring::IoUring::new(8).unwrap();
    let (submitter, mut submission, mut completion) = ring.split();

    if fixed_files {
        submitter.register_files(&[socket.as_raw_fd()]).unwrap();
    }

    let buf_ring_mmap = BufRingMmap::new(8).unwrap();
    let mut buf_ring = BufRing::register(&submitter, 0, buf_ring_mmap).unwrap();

    const BUF_SIZE: usize = 16;

    let mut bufs = Vec::new();
    let mut iovecs = Vec::new();

    for i in 0..buf_ring.entry_count() {
        let mut buf = Box::new([1u8; BUF_SIZE]);

        bufs.push(buf.as_mut_ptr());

        iovecs.push(libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: buf.len(),
        });

        // Note: this leaks the memory.
        unsafe {
            buf_ring.add_buffer(Box::into_raw(buf), i);
        }
    }

    if fixed_buffers {
        unsafe {
            submitter.register_buffers(&iovecs).unwrap();
        }
    }

    {
        const FUTEX2_SIZE_U32: u32 = 0x2;
        let wait = io_uring::opcode::FutexWait::new(
            stop.as_ptr() as *const _,
            0,
            libc::FUTEX_BITSET_MATCH_ANY as c_uint as u64,
            FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as u32,
        )
        .build()
        .user_data(1);
        unsafe {
            submission.push(&wait).unwrap();
        }
        submission.sync();
    }

    let addr = libc::sockaddr_in {
        sin_family: u16::try_from(libc::AF_INET).unwrap(),
        sin_port: 12000u16.to_be(),
        sin_addr: libc::in_addr {
            s_addr: libc::INADDR_LOOPBACK.to_be(),
        },
        sin_zero: [0; 8],
    };

    let mut in_flight = 0;
    let mut datagram_count = 0;

    'main_loop: loop {
        let keep_sending = stop.load(Ordering::Relaxed) == 0;
        if keep_sending {
            while !submission.is_full() {
                let len = u32::try_from(BUF_SIZE).unwrap();

                let entry = if zero_copy {
                    let mut send = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::SendZc::new(fixed, bufs[0], len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::SendZc::new(fd, bufs[0], len)
                    };

                    if fixed_buffers {
                        send = send.buf_index(Some(0));
                    }

                    send.dest_addr(&addr as *const libc::sockaddr_in as *const _)
                        .dest_addr_len(u32::try_from(mem::size_of_val(&addr)).unwrap())
                        .build()
                } else {
                    let send = if fixed_files {
                        let fixed = io_uring::types::Fixed(0);
                        io_uring::opcode::Send::new(fixed, bufs[0], len)
                    } else {
                        let fd = io_uring::types::Fd(socket.as_raw_fd());
                        io_uring::opcode::Send::new(fd, bufs[0], len)
                    };

                    if fixed_buffers {
                        panic!("fixed buffers is only supported when zero-copy is enabled");
                    }

                    send.dest_addr(&addr as *const libc::sockaddr_in as *const _)
                        .dest_addr_len(u32::try_from(mem::size_of_val(&addr)).unwrap())
                        .build()
                };

                unsafe {
                    submission.push(&entry).unwrap();
                }

                in_flight += 1;
            }

            submission.sync();
        } else if in_flight == 0 {
            break;
        }

        completion.sync();

        for entry in &mut completion {
            if entry.user_data() == 1 {
                if entry.result() < 0 {
                    eprintln!("futex_wait: {}", entry.result());
                }
                break 'main_loop;
            }
            if io_uring::cqueue::notif(entry.flags()) {
                continue;
            }
            if entry.result() < 0 {
                eprintln!("sendto: {}", entry.result());
                break 'main_loop;
            }
            datagram_count += 1;
            in_flight -= 1;
        }

        submitter.submit_and_wait(1).unwrap();
    }

    datagram_count
}

fn receive_datagrams(
    socket: &UdpSocket,
    fixed_files: bool,
    fixed_buffers: bool,
    stop: &AtomicU32,
) -> u64 {
    let mut ring = io_uring::IoUring::new(8).unwrap();
    let (submitter, mut submission, mut completion) = ring.split();

    if fixed_files {
        submitter.register_files(&[socket.as_raw_fd()]).unwrap();
    }

    let buf_ring_mmap = BufRingMmap::new(8).unwrap();
    let mut buf_ring = BufRing::register(&submitter, 0, buf_ring_mmap).unwrap();

    const BUF_SIZE: usize = 16;

    let mut bufs = Vec::new();
    let mut iovecs = Vec::new();

    for i in 0..buf_ring.entry_count() {
        let mut buf = Box::new([1u8; BUF_SIZE]);

        bufs.push(buf.as_mut_ptr());

        iovecs.push(libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: buf.len(),
        });

        // Note: this leaks the memory.
        unsafe {
            buf_ring.add_buffer(Box::into_raw(buf), i);
        }
    }

    if fixed_buffers {
        unsafe {
            submitter.register_buffers(&iovecs).unwrap();
        }
    }

    {
        const FUTEX2_SIZE_U32: u32 = 0x2;
        let wait = io_uring::opcode::FutexWait::new(
            stop.as_ptr() as *const _,
            0,
            libc::FUTEX_BITSET_MATCH_ANY as c_uint as u64,
            FUTEX2_SIZE_U32 | libc::FUTEX_PRIVATE_FLAG as c_uint,
        )
        .build()
        .user_data(1);
        unsafe {
            submission.push(&wait).unwrap();
        }
        submission.sync();
    }

    let mut in_flight = 0;
    let mut datagram_count = 0;

    'main_loop: loop {
        let keep_receiving = stop.load(Ordering::Relaxed) == 0;
        if keep_receiving {
            while !submission.is_full() {
                let len = u32::try_from(BUF_SIZE).unwrap();

                let entry = if fixed_files {
                    let fixed = io_uring::types::Fixed(0);
                    io_uring::opcode::Recv::new(fixed, bufs[0], len).build()
                } else {
                    let fd = io_uring::types::Fd(socket.as_raw_fd());
                    io_uring::opcode::Recv::new(fd, bufs[0], len).build()
                };

                unsafe {
                    submission.push(&entry).unwrap();
                }

                in_flight += 1;
            }

            submission.sync();
        } else if in_flight == 0 {
            break;
        }

        completion.sync();

        for entry in &mut completion {
            if entry.user_data() == 1 {
                if entry.result() < 0 {
                    eprintln!("futex_wait: {}", entry.result());
                }
                break 'main_loop;
            }
            if io_uring::cqueue::notif(entry.flags()) {
                continue;
            }
            if entry.result() < 0 {
                eprintln!("recv: {}", entry.result());
                return datagram_count;
            }
            datagram_count += 1;
            in_flight -= 1;
        }

        submitter.submit_and_wait(1).unwrap();
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

    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();

    let thread_count = args
        .threads
        .unwrap_or_else(|| thread::available_parallelism().unwrap());

    mask_sigint().expect("failed to mask SIGINT");

    let signalfd = signalfd_full(libc::SFD_CLOEXEC).expect("failed to create signalfd");

    let mut ring = io_uring::IoUring::new(8).expect("failed to create io_uring instance");

    let mut siginfo: MaybeUninit<libc::signalfd_siginfo> = MaybeUninit::uninit();

    {
        let timespec = Timespec::new().sec(1);
        let timeout = io_uring::opcode::Timeout::new(&timespec)
            .flags(TimeoutFlags::BOOTTIME)
            .build();

        let fd = io_uring::types::Fd(signalfd.as_raw_fd());
        // TOOD: Will this cause UB due to pointer/reference aliasing rules?
        let read = io_uring::opcode::Read::new(
            fd,
            siginfo.as_mut_ptr() as *mut _,
            u32::try_from(mem::size_of_val(&siginfo)).unwrap(),
        )
        .build();

        unsafe {
            ring.submission().push_multiple(&[timeout, read]).unwrap();
        }

        ring.submitter()
            .submit()
            .expect("failed to submit SQEs on main io_uring instance");
    }

    let stop = AtomicU32::new(0);

    let datagram_count: u64 = thread::scope(|s| {
        let mut threads = Vec::new();

        for _ in 0..thread_count.get() {
            threads.push(s.spawn(|| {
                if args.server {
                    receive_datagrams(&socket, args.fixed_files, args.fixed_buffers, &stop)
                } else {
                    send_datagrams(
                        &socket,
                        args.fixed_files,
                        args.fixed_buffers,
                        args.zero_copy,
                        &stop,
                    )
                }
            }));
        }

        loop {
            ring.submitter()
                .submit_and_wait(1)
                .expect("failed to wait on main io_uring instance");
            if ring.completion().next().is_some() {
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
