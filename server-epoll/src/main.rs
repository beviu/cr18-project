use std::{
    io,
    mem::MaybeUninit,
    net::TcpListener,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    ptr,
};

use clap::Parser;

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    bind: String,
}

fn epoll_create1(flags: i32) -> io::Result<OwnedFd> {
    let ret = unsafe { libc::epoll_create1(flags) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(ret) })
}

fn epoll_ctl_add(
    epoll_fd: &impl AsRawFd,
    fd: &impl AsRawFd,
    event: &libc::epoll_event,
) -> io::Result<()> {
    let ret = unsafe {
        libc::epoll_ctl(
            epoll_fd.as_raw_fd(),
            libc::EPOLL_CTL_ADD,
            fd.as_raw_fd(),
            event as *const _ as *mut _,
        )
    };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn epoll_ctl_del(epoll_fd: &impl AsRawFd, fd: &impl AsRawFd) -> io::Result<()> {
    let ret = unsafe {
        libc::epoll_ctl(
            epoll_fd.as_raw_fd(),
            libc::EPOLL_CTL_DEL,
            fd.as_raw_fd(),
            ptr::null_mut(),
        )
    };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn epoll_wait(
    epoll_fd: &impl AsRawFd,
    events: *mut libc::epoll_event,
    capacity: i32,
    timeout: i32,
) -> io::Result<i32> {
    let ret = unsafe { libc::epoll_wait(epoll_fd.as_raw_fd(), events, capacity, timeout) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret)
}

fn read(fd: &impl AsRawFd, buf: &mut [MaybeUninit<u8>]) -> io::Result<isize> {
    let ret = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret)
}

fn handle_event(event: &libc::epoll_event, epoll_fd: BorrowedFd, socket: &TcpListener) {
    // The user data is `u64::MAX` for the server socket and the client file descriptor for client
    // sockets.
    if event.u64 == u64::MAX {
        loop {
            let (client, _addr) = match socket.accept() {
                Ok(ret) => ret,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to accept: {err}"),
            };

            epoll_ctl_add(
                &epoll_fd,
                &client,
                &libc::epoll_event {
                    events: libc::EPOLLIN as u32,
                    u64: client.as_raw_fd() as u64,
                },
            )
            .unwrap();

            let _fd = client.into_raw_fd();
        }
    } else {
        let fd: RawFd = event.u64 as i32;

        loop {
            let mut buf = [MaybeUninit::uninit(); 4096];
            let n = match read(&fd, &mut buf) {
                Ok(ret) => ret,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to read: {err}"),
            };
            if n == 0 {
                epoll_ctl_del(&epoll_fd, &fd).unwrap();
                drop(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
    }
}

fn main() {
    let args = Args::parse();

    let socket = TcpListener::bind(&args.bind).unwrap();
    socket.set_nonblocking(true).unwrap();

    let epoll_fd = epoll_create1(libc::EPOLL_CLOEXEC).unwrap();

    epoll_ctl_add(
        &epoll_fd,
        &socket,
        &libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: u64::MAX,
        },
    )
    .unwrap();

    let mut events = Vec::with_capacity(1024);

    loop {
        let n = epoll_wait(&epoll_fd, events.as_mut_ptr(), events.capacity() as i32, 0).unwrap();
        unsafe {
            events.set_len(n as usize);
        }

        for event in &events {
            handle_event(&event, epoll_fd.as_fd(), &socket);
        }
    }
}
