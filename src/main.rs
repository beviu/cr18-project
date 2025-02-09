use std::{
    ffi::c_void,
    io, mem,
    net::UdpSocket,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

use buf_ring::{BufRing, BufRingMmap};
use clap::Parser;

mod buf_ring;

#[derive(clap::Parser)]
struct Args {
    /// Use fixed files instead of file descriptors.
    #[clap(short, long)]
    fixed_files: bool,

    /// Use a buffer ring.
    #[clap(short, long)]
    buf_ring: bool,
}

fn main() {
    let args = Args::parse();

    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();

    let mut ring = io_uring::IoUring::new(8).unwrap();
    let (submitter, mut submission, mut completion) = ring.split();

    submitter.register_files(&[socket.as_raw_fd()]).unwrap();

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

    unsafe {
        submitter.register_buffers(&iovecs).unwrap();
    }

    let start = Instant::now();
    let mut datagram_count = 0;

    let addr = libc::sockaddr_in {
        sin_family: u16::try_from(libc::AF_INET).unwrap(),
        sin_port: 12000u16.to_be(),
        sin_addr: libc::in_addr {
            s_addr: libc::INADDR_LOOPBACK.to_be(),
        },
        sin_zero: [0; 8],
    };

    let mut in_flight = 0;

    loop {
        let keep_sending = start.elapsed() < Duration::from_secs(1);
        if keep_sending {
            while !submission.is_full() {
                let len = u32::try_from(BUF_SIZE).unwrap();
                let send = if args.fixed_files {
                    let fixed = io_uring::types::Fixed(0);
                    io_uring::opcode::SendZc::new(fixed, bufs[0], len)
                } else {
                    let fd = io_uring::types::Fd(socket.as_raw_fd());
                    io_uring::opcode::SendZc::new(fd, bufs[0], len)
                };

                let entry = send
                    .buf_index(Some(0))
                    .dest_addr(&addr as *const libc::sockaddr_in as *const _)
                    .dest_addr_len(u32::try_from(mem::size_of_val(&addr)).unwrap())
                    .build();
                unsafe {
                    submission.push(&entry).unwrap();
                }

                in_flight += 1;
            }

            submission.sync();
        } else if in_flight == 0 {
            break;
        }

        if in_flight > 0 {
            completion.sync();

            for entry in &mut completion {
                if entry.result() < 0 {
                    eprintln!("sendto: {}", entry.result());
                    return;
                }
                datagram_count += 1;
                in_flight -= 1;
            }
        }

        if in_flight > 0 {
            submitter.submit_and_wait(1).unwrap();
        } else {
            submitter.squeue_wait().unwrap();
        }
    }

    println!("basic: {datagram_count}");
}
