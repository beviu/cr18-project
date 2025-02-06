use std::{
    mem,
    net::UdpSocket,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

use clap::Parser;

#[derive(clap::Parser)]
struct Args {
    /// Use fixed files instead of file descriptors.
    #[clap(short, long)]
    fixed_files: bool,
}

fn main() {
    let args = Args::parse();

    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let mut ring = io_uring::IoUring::new(8).unwrap();

    ring.submitter()
        .register_files(&[socket.as_raw_fd()])
        .unwrap();

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
        const BUF_SIZE: usize = 16;

        let keep_sending = start.elapsed() < Duration::from_secs(1);
        if keep_sending {
            let mut submission = ring.submission();
            while !submission.is_full() {
                let buf = Box::new([0u8; BUF_SIZE]);

                let len = u32::try_from(buf.len()).unwrap();
                let buf = Box::into_raw(buf) as *mut u8;
                let send = if args.fixed_files {
                    let fixed = io_uring::types::Fixed(0);
                    io_uring::opcode::Send::new(fixed, buf, len)
                } else {
                    let fd = io_uring::types::Fd(socket.as_raw_fd());
                    io_uring::opcode::Send::new(fd, buf, len)
                };

                let entry = send
                    .dest_addr(&addr as *const libc::sockaddr_in as *const _)
                    .dest_addr_len(u32::try_from(mem::size_of_val(&addr)).unwrap())
                    .build()
                    .user_data(buf as u64);
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
            let mut completion = ring.completion();
            completion.sync();

            for entry in completion {
                let _buf = unsafe { Box::from_raw(entry.user_data() as *mut [u8; BUF_SIZE]) };
                if entry.result() < 0 {
                    eprintln!("sendto: {}", entry.result());
                    return;
                }
                datagram_count += 1;
                in_flight -= 1;
            }
        }

        if in_flight > 0 {
            ring.submitter().submit_and_wait(1).unwrap();
        } else {
            ring.submitter().squeue_wait().unwrap();
        }
    }

    println!("basic: {datagram_count}");
}
