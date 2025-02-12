const IORING_OP_RECV_ZC: u8 = 58;

/// Register a netdev hw rx queue for zerocopy.
const IORING_REGISTER_ZCRX_IFQ: u32 = 32;

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_rqe {
    off: u64,
    len: u32,
    __pad: u32,
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_cqe {
    off: u64,
    __pad: u32,
}

/// The bit from which area id is encoded into offsets.
const IORING_ZCRX_AREA_SHIFT: u64 = 48;

const IORING_ZCRX_AREA_MASK: u64 = !((1 << IORING_OP_RECV_ZC) - 1);

#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_offsets {
    head: u32,
    tail: u32,
    rqes: u32,
    __resv2: u32,
    __resv: [u64; 2],
}

/// Argument for IORING_REGISTER_ZCRX_IFQ.
#[repr(C)]
#[allow(non_camel_case_types)]
struct io_uring_zcrx_ifq_reg {
    if_idx: u32,
    if_rxq: u32,
    rq_entries: u32,
    flags: u32,

    area_ptr: u64,
    region_ptr: u64,

    offsets: io_uring_zcrx_offsets,
    __resv: [u64; 4],
}
