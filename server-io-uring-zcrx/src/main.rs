use std::{ffi::CString, io, net::TcpListener, os::fd::AsRawFd};

use clap::Parser;
use io_uring::{
    cqueue,
    opcode::{AcceptMulti, FilesUpdate, RecvZcMulti},
    squeue,
    types::Fixed,
    IoUring, SubmissionQueue,
};
use io_uring_zcrx::{IoUringZcrxIfq, ZcrxCqe};

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    bind: String,

    #[clap(short, long)]
    interface: String,

    #[clap(short, long)]
    queue: u32,
}

fn handle_completion(
    cqe: &cqueue::Entry32,
    sq: &mut SubmissionQueue<squeue::Entry>,
    zcrx_ifq: &mut IoUringZcrxIfq,
) {
    if cqe.user_data() == u64::MAX {
        // FILES_UPDATE operation to unregister a client.
        return;
    }

    // To make things simpler, the user data in SQEs will represent the file index of the
    // server or client socket.
    let file_index = cqe.user_data() as u32;
    if file_index == 0 {
        let ret = cqe.result();
        if ret < 0 {
            panic!("accept failed: {ret}");
        }
        let file_index = ret as u32;
        let recv = RecvZcMulti::new(Fixed(file_index))
            .build()
            .user_data(file_index.into());
        unsafe {
            sq.push(&recv).unwrap();
        }
    } else {
        let ret = cqe.result();
        if ret < 0 {
            eprintln!("recv failed: {ret}");
        }
        if ret <= 0 {
            // Unregister the client socket.
            const DELETE: i32 = -1;
            let unregister = FilesUpdate::new(&DELETE as *const _, 1)
                .offset(file_index as i32)
                .build()
                .user_data(u64::MAX);
            unsafe {
                sq.push(&unregister).unwrap();
            }
        } else {
            let available_len = ret as usize;
            let rcqe = ZcrxCqe::from(cqe.clone());
            assert_eq!(rcqe.area_token(), 0);
            let buf = unsafe {
                zcrx_ifq
                    .get_buf(rcqe.buffer_offset(), available_len)
                    .unwrap()
            };
            let rqe = buf.into_refill_entry();
            unsafe { zcrx_ifq.refill().push(&rqe).unwrap() };
        }
    }
}

fn main() {
    let args = Args::parse();

    let interface_cstring = CString::new(args.interface).unwrap();
    let interface_index = unsafe { libc::if_nametoindex(interface_cstring.as_c_str().as_ptr()) };
    if interface_index == 0 {
        let err = io::Error::last_os_error();
        panic!("failed to convert interface name: {err}");
    }

    let mut io_uring = IoUring::builder()
        .build(32)
        .expect("failed to create io_uring instance");

    let listener = TcpListener::bind(&args.bind).unwrap();

    let submitter = io_uring.submitter();
    // Register a big file table to store server and client sockets.
    submitter.register_files_sparse(128).unwrap();
    submitter
        .register_files_update(0, &[listener.as_raw_fd()])
        .unwrap();

    let accept = AcceptMulti::new(Fixed(0)).allocate_file_index(true).build();
    unsafe {
        io_uring.submission().push(&accept).unwrap();
    }

    let mut zcrx_ifq =
        IoUringZcrxIfq::register(&io_uring, interface_index, args.queue, 32, 16384).unwrap();

    loop {
        let (submitter, mut sq, cq) = io_uring.split();
        for cqe in cq {
            handle_completion(&cqe, &mut sq, &mut zcrx_ifq);
        }
        // Synchronize the submission queue with the kernel.
        drop(sq);
        submitter.submit_and_wait(1).unwrap();
    }
}
