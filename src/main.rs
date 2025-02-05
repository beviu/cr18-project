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
    while start.elapsed() < Duration::from_secs(1) {
        let buf = [0u8; 16];

        let addr = libc::sockaddr_in {
            sin_family: u16::try_from(libc::AF_INET).unwrap(),
            sin_port: 12000u16.to_be(),
            sin_addr: libc::in_addr {
                s_addr: libc::INADDR_LOOPBACK.to_be(),
            },
            sin_zero: [0; 8],
        };

        let len = u32::try_from(buf.len()).unwrap();
        let buf = &buf as *const u8;
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
            .build();

        unsafe {
            ring.submission().push(&entry).unwrap();
        }
        ring.submit_and_wait(1).unwrap();

        let mut completion = ring.completion();
        completion.sync();
        for entry in completion {
            if entry.result() < 0 {
                eprintln!("sendto: {}", entry.result());
                return;
            }
            datagram_count += 1;
        }
    }
    println!("basic: {datagram_count}");
}
