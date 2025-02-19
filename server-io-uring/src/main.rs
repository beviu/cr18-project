use std::{net::TcpListener, os::fd::AsRawFd};

use clap::Parser;
use io_uring::{
    cqueue,
    opcode::{AcceptMulti, FilesUpdate, RecvMulti},
    squeue,
    types::Fixed,
    IoUring, SubmissionQueue,
};
use io_uring_buf_ring::IoUringBufRing;

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    bind: String,
}

fn handle_completion(
    cqe: &cqueue::Entry,
    sq: &mut SubmissionQueue<squeue::Entry>,
    buf_ring: &mut IoUringBufRing<Vec<u8>>,
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
        let recv = RecvMulti::new(Fixed(file_index), 0)
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
            let id = cqueue::buffer_select(cqe.flags()).unwrap();
            let available_len = ret as usize;
            let _buf = unsafe { buf_ring.get_buf(id, available_len) };
        }
    }
}

fn main() {
    let args = Args::parse();

    let mut io_uring = IoUring::builder()
        .setup_coop_taskrun()
        .setup_defer_taskrun()
        .setup_single_issuer()
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

    let mut buf_ring = IoUringBufRing::new(&io_uring, 16, 0, 4096).unwrap();

    loop {
        let (submitter, mut sq, cq) = io_uring.split();
        for cqe in cq {
            handle_completion(&cqe, &mut sq, &mut buf_ring);
        }
        // Synchronize the submission queue with the kernel.
        drop(sq);
        submitter.submit_and_wait(1).unwrap();
    }
}
