use std::{
    mem,
    net::UdpSocket,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

fn main() {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let mut ring = io_uring::IoUring::new(8).unwrap();
    let start = Instant::now();
    let mut datagram_count = 0;
    while start.elapsed() < Duration::from_secs(1) {
        let fd = io_uring::types::Fd(socket.as_raw_fd());
        let buf = [0u8; 16];

        let addr = libc::sockaddr_in {
            sin_family: u16::try_from(libc::AF_INET).unwrap(),
            sin_port: 12000u16.to_be(),
            sin_addr: libc::in_addr {
                s_addr: libc::INADDR_LOOPBACK.to_be(),
            },
            sin_zero: [0; 8],
        };

        let entry =
            io_uring::opcode::Send::new(fd, &buf as *const _, u32::try_from(buf.len()).unwrap())
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
