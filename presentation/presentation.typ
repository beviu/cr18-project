#import "@preview/touying:0.6.1": *
#import themes.simple: simple-theme, title-slide
#import "@preview/cetz:0.3.2"
#import "@preview/cetz-plot:0.1.1"

#show: simple-theme.with(
  primary: fuchsia,
  author: [Greg Depoire-\-Ferrer],
  config-info(
    title: [Benchmarking packet reception with `AF_XDP`, DPDK and `io_uring`],
    author: [Greg Depoire-\-Ferrer],
    date: datetime(day: 3, month: 3, year: 2025),
    institution: [ENS de Lyon],
  ),
)

#show link: set text(fill: fuchsia)

#title-slide[
  = Benchmarking packet reception with `AF_XDP`, DPDK and `io_uring`

  #v(1em)

  Greg Depoire-\-Ferrer
]

= Motivation

== The performance of networking stacks

=== Why use a different networking stack?

// Touying has a bug
// (https://forum.typst.app/t/how-do-i-add-bibliography-to-a-touying-presentation/643/7) which makes
// this code snippet create a blank slide if it's put in the preamble, so put it later.
#show: magic.bibliography-as-footnote.with(
  bibliography("bibliography.yaml", title: none),
)

- A need for processing packets on *commodity hardware* with *low overhead*.@retina
- *Portability* and integration with the operating system versus *performance*.

=== `io_uring`

Keeps evolving #math.arrow.r need for new benchmarks.

== Comparisons between networking stacks

Networking stacks support different layers.

=== Ethernet

- `AF_XDP`
- DPDK

=== TCP

- `epoll`
- `io_uring`

= Ethernet

== DPDK

=== Description

- Kernel bypass #math.arrow.r no overhead due to transition between kernel space and user space.
- Networking code is written to be as fast as possible #math.arrow.r faster than Linux networking stack.
- Need to reserve an entire NIC for the application #math.arrow.r no sharing of resources.
- Linux NIC drivers cannot be reused.

#align(right, image("DPDK_logo_horizontal.svg", height: 1fr))

== Express Data Path (XDP)

=== Description

- *XDP programs* are BPF programs that are called for every incoming packet just after reception but before allocating memory for a socket buffer.
- They can drop packets, modify them, and chose to pass them to the networking stack, redirect them to a port or to userspace for further processing.
- Packets redirected to userspace are received on a `AF_XDP` socket.
- Need to reserve a NIC queue for the application.
- Uses Linux NIC drivers.

== Experiment

- _Sender_ machine sends packets as fast as possible to _receiver_ machine using Pktgen-DPDK.
- Measure the maximum number of packets received per second.
- Vary the number of *cores* and *packet size*.

#align(
  center + horizon,
  cetz.canvas({
    import cetz.draw: *

    content((), frame: "rect", padding: .5em, [Sender], name: "sender")
    content(
      (rel: (5, 0), to: "sender.east"),
      frame: "rect",
      padding: .5em,
      [Receiver],
      anchor: "west",
      name: "receiver",
    )

    line("sender", "receiver", mark: (end: ">"), name: "arrow")
    content((rel: (0, -.5em), to: "arrow"), [Frames], anchor: "north")
  }),
)

== Results

#align(
  center + horizon,
  cetz.canvas({
    import cetz.draw: *
    import cetz-plot: chart

    let data = (
      ([64], 2.396386, .337939),
      ([128], 2.312816, .325026),
      ([256], 2.287169, .327398),
      ([512], 2.247936, .399457),
      ([768], 1.585184, .370436),
      ([1024], 1.136224, .296402),
      ([1280], .962752, .239992),
      ([1518], .814688, .203063),
    )

    chart.barchart(
      mode: "clustered",
      size: (16, auto),
      label-key: 0,
      value-key: (1, 2),
      data,
      labels: ([VM (DPDK)], [VM (`AF_XDP`)]),
      x-label: [Maximum Mpps received],
      y-label: [Packet size (bytes)],
    )
  }),
)

== Conclusions

- `AF_XDP` performs poorly compared to DPDK and has more variability.
- Packet size is less important with `AF_XDP` than DPDK #math.arrow.r the overhead is not in memory copying.
- It's possible that I made a mistake in the setup.
- I was not able to test on bare-metal, but I think these results can still be useful for cloud
  providers that run everything in VMs.

= TCP

== BSD socket API

- Create a socket: `socket`, `accept`
- Receiving data: `recv`, `recvfrom`, `read`
- Sending data: `send`, `sendto`, `write`

=== Characteristics

- One system call per operation
- *Blocking*: no compute or IO can be done in the meanwhile

#pagebreak()

=== Example

```python
import random
import socket

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(('0.0.0.0', 53))

while True:
    datagram, address = s.recvfrom(2048)
    print(f'Received {datagram} from {address}.')
```

== `select` and friends

#columns(
  2,
  [
    #text(
      13pt,
      [
        ```python
        import socket, select

        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.bind(('0.0.0.0', 80))
        s.listen(8)

        sockets = [s]

        while True:
            ready, _, _ = select.select(sockets, [], [])

        for r in ready:
            if r == s:
                c, addr = s.accept()
                sockets.append(c)
            else:
                data = sock.recv(2048)
                if data:
                    print(f'Received {data} from {r}')
                else:
                    l.remove(r)
        ```
      ],
    )

    - *Readiness-based*
    - One system call per operation
    - *Non-blocking*
    - Variants: `poll`, *`epoll`*, `kqueue`
  ],
)

=== Readiness-based interface

```c
/* s is ready... */

for (;;) {
    char buf[2048];

    ssize_t n = recv(s, buf, sizeof(buf), 0);
    if (n == 0)
        break;

    process(buf, n);
}
```

== `epoll`

- Also *readiness-based*
- Modern version of `select` with less copying
- FreeBSD equivalent: `kqueue`

== `io_uring`

- *Completion-based*
- Start many operations with a single system call

=== Completion-based interface

```c
char *buf = malloc(2048);
start_read_operation(s, buf, sizeof(buf), 1234);

/* Later... */

int id = process_completed_operation();
printf("%d\n", id); // 1234
```

=== Presentation

#align(center + horizon, image("io_uring.png", height: 1fr))

#pagebreak()

=== Performance improvements

- Fixed files and buffers (May 2019)
- Buffer ring (July 2022)
- Zero-copy reception (not merged yet!)

#pagebreak()

=== Fixed files

Pre-register FDs with the ring to reduce the overhead of:
- reference counting, and
- descriptor table lookup.

```c
int io_uring_register_files(
    struct io_uring *ring,
    const int *files,
    unsigned nr_files,
);
```

#pagebreak()

=== Buffer ring

Instead of specifying a buffer to use in `read` operations, let the kernel pick a buffer from another ring buffer.

```c
struct io_uring_buf_ring *io_uring_setup_buf_ring(
    struct io_uring *ring,
    unsigned int nentries,
    int bgid,
    unsigned int flags,
    int *ret,
);
```

#pagebreak()

=== Zero-copy reception

A new receive operation where the NIC directly writes to application memory.

#pagebreak()

#let column(a, b, rows) = {
  import cetz.draw: *

  get-ctx(ctx => {
    let (ctx, a, b) = cetz.coordinate.resolve(ctx, a, b)

    assert(
      a.at(2) == b.at(2),
      message: "Both rectangle points must have the same z value.",
    )

    let min = (
      calc.min(a.at(0), b.at(0)),
      calc.min(a.at(1), b.at(1)),
      calc.min(a.at(2), b.at(2)),
    )
    let max = (
      calc.max(a.at(0), b.at(0)),
      calc.max(a.at(1), b.at(1)),
      calc.max(a.at(2), b.at(2)),
    )

    let row-height = (max.at(1) - min.at(1)) / rows.len()

    for (i, paint) in rows.enumerate() {
      let y = min.at(1) + i * row-height
      rect(
        (min.at(0), y, min.at(2)),
        (max.at(0), y + row-height, max.at(2)),
        fill: paint,
      )
    }
  })
}

#let ring(position, inner-radius, outer-radius, parts, cursors: ()) = {
  import cetz.draw: *

  group({
    translate(position)

    let thickness = outer-radius - inner-radius

    let part-count = 9
    let part-length = 360deg / parts.len()

    for (i, paint) in parts.enumerate() {
      let angle = 90deg - i * part-length
      arc(
        (0, 0),
        delta: part-length,
        stop: angle,
        stroke: (thickness: thickness, paint: paint),
        anchor: "origin",
        radius: (inner-radius + outer-radius) / 2,
      )
    }

    circle((0, 0), radius: inner-radius)
    circle((0, 0), radius: outer-radius)

    line((0, inner-radius), (0, outer-radius))

    for i in range(parts.len()) {
      let angle = 90deg - i * part-length
      let x = calc.cos(angle)
      let y = calc.sin(angle)
      line(
        (x * inner-radius, y * inner-radius),
        (x * outer-radius, y * outer-radius),
      )
    }

    for (body, position) in cursors {
      let angle = 90deg - position * part-length
      group({
        rotate(angle)
        content(
          (outer-radius + 0.7, 0),
          text(12pt, body),
          name: "tail-label",
          angle: angle - 90deg,
          anchor: "south",
        )
        line(
          (outer-radius + 0.6, 0),
          (outer-radius + 0.1, 0),
          mark: (end: ">"),
        )
      })
    }
  })
}

#let zc-diagram(
  area-buffer-color: silver,
  head-position: 0,
  tail-position: 0,
) = cetz.canvas({
  import cetz.draw: *

  column(
    (0, 0),
    (3, 4),
    (silver, silver, area-buffer-color, silver, silver, silver),
  )
  content((1.5, -1), [Area], anchor: "north")

  let cursors = if head-position == tail-position {
    ((align(center, [head, \ tail]), head-position),)
  } else {
    (
      ([head], head-position),
      ([tail], tail-position),
    )
  }

  let parts = (
    range(head-position).map(_ => silver)
      + range(head-position, tail-position).map(_ => blue)
      + range(tail-position, 9).map(_ => silver)
  )

  ring(
    (6, 2),
    0.75,
    1.25,
    parts,
    cursors: cursors,
  )
  content((6, -1), align(center, [Refill \ buffer]), anchor: "north")
})

#align(
  horizon,
  grid(
    columns: (1fr, 2fr),
    gutter: 2em,
    alternatives(
      zc-diagram(),
      zc-diagram(area-buffer-color: teal),
      zc-diagram(area-buffer-color: green),
      zc-diagram(area-buffer-color: green),
      zc-diagram(area-buffer-color: green),
      zc-diagram(area-buffer-color: green, tail-position: 1),
      zc-diagram(head-position: 1, tail-position: 1),
    ),
    [
      + App submits `RECV_ZC` operation. #pause
      + Kernel picks free buffer in area. #pause
      + NIC writes to buffer. #pause
      + Kernel enqueues completion. #pause
      + App processes data in buffer. #pause
      + App enqueues buffer ready to be reused. #pause
      + Kernel marks buffer as available.
    ],
  ),
)

#pagebreak()

=== Support

This work is not merged yet into Linux and `liburing` (a library to use `io_uring`). The patch
series have some example code, but not much other than that.

I created a Rust wrapper to setup the area and refill buffer needed to use the new `RECV_ZC`
operation: https://github.com/beviu/io-uring-zcrx.

I modified the `io-uring` Rust crate to add support for this new operation:
https://github.com/beviu/io-uring/compare/master...zcrx.

== Experiment

- _Sender_ machine sends data on TCP socket as fast as possible to _receiver_ machine.
- Two TCP server implementations (see
  #link("https://github.com/beviu/cr18-project/tree/master/server-io-uring")[`server-io-uring`]
  and #link("https://github.com/beviu/cr18-project/tree/master/server-epoll")[`server-epoll`]
  directories).
- Measure the bandwidth.

#align(
  center + horizon,
  cetz.canvas({
    import cetz.draw: *

    content((), frame: "rect", padding: .5em, [Sender], name: "sender")
    content(
      (rel: (5, 0), to: "sender.east"),
      frame: "rect",
      padding: .5em,
      [Receiver],
      anchor: "west",
      name: "receiver",
    )

    line("sender", "receiver", mark: (end: ">"), name: "arrow")
    content((rel: (0, -.5em), to: "arrow"), [Segments], anchor: "north")
  }),
)

== Results

#align(center + horizon)[
  Unfortunately, I was not able to run the benchmarks.
]

#appendix[
  = Appendix

  == Virtual machine setup <vm-setup>

  I prepared the benchmark setup beforehand, and tested it in virtual
  machines on my PC first to make sure I was ready before testing on actual
  #link("https://www.grid5000.fr/w/Grid5000:Home")[Grid5000] hardware. Unfortunately, in the end, I
  was not able to test on Grid5000.

  I installed #link("https://archlinux.org/")[Arch Linux] in two VMs and configured a bridge with a
  tap device for each VM in the host. I used `virtio-net` NICs.

  #pagebreak()

  === Host network interfaces setup

  #grid(
    columns: (1fr, auto),
    [
      ```sh
      ip tuntap add mode tap tap0
      ip tuntap add mode tap tap1
      ip link add name br0 type bridge
      ip link set dev br0 up
      ip link set tap0 up
      ip link set tap1 up
      ip link set tap0 master br0
      ip link set tap1 master br0
      ip addr add 10.0.0.1/24 dev br0
      ```
    ],
    align(
      horizon,
      text(
        18pt,
        cetz.canvas({
          import cetz.draw: *

          content(
            (),
            frame: "rect",
            padding: .5em,
            align(center)[Sender \ `10.0.0.2`],
            name: "sender",
          )
          content(
            (rel: (1, 0), to: "sender.east"),
            frame: "rect",
            padding: .5em,
            align(center)[Switch],
            anchor: "west",
            name: "switch",
          )
          content(
            (rel: (0, -1), to: "switch.south"),
            frame: "rect",
            padding: .5em,
            align(center)[Router \ `10.0.0.1`],
            anchor: "north",
            name: "router",
          )
          content(
            (rel: (1, 0), to: "switch.east"),
            frame: "rect",
            padding: .5em,
            align(center)[Receiver \ 10.0.0.3],
            anchor: "west",
            name: "receiver",
          )

          line("switch", "sender", mark: (symbol: ">"), name: "arrow")
          line("switch", "router", mark: (symbol: ">"), name: "arrow")
          line("switch", "receiver", mark: (symbol: ">"), name: "arrow")
        }),
      ),
    ),
  )

  Configure a NAT to give the tap devices access to the Internet:

  ```sh
  sysctl net.ipv4.ip_forward=1
  nft add table nat
  nft 'add chain nat postrouting { type nat hook postrouting priority 100 ; }'
  nft add rule nat postrouting ip saddr 10.0.0.0/24 masquerade
  ```

  #pagebreak()

  === QEMU command line

  Make sure to replace the tap interface name and MAC address for the receiver VM. The arguments for
  specifying the drives were omitted.

  #text(
    18pt,
    ```sh
    qemu-system-x86_64 \
      -machine type=q35,accel=kvm,kernel-irqchip=split \
      -cpu host -smp 4 -m 2G \
      -device intel-iommu,intremap=on,caching-mode=on \
      -vga none \
      -serial mon:stdio \
      -monitor none \
      -nographic \
      -netdev tap,id=tap,ifname=tap0,vhost=on,script=no,downscript=no \
      -device virtio-net-pci,mq=on,vectors=2,netdev=tap,mac=52:54:00:f8:e2:e3
    ```,
  )

  === Kernel configuration

  I followed the instructions from the
  #link("https://doc.dpdk.org/guides/nics/virtio.html#prerequisites-for-rx-interrupts")[DPDK
documentation].

  I compiled the kernel at commit `76544811c850a1f4c055aa182b513b7a843868ea` with a
  #link("https://github.com/beviu/cr18-project/tree/main/kernel-config")[custom configuration] and
  added `console=ttyS0,11520 intel_iommu=on vfio.enable_unsafe_noiommu_mode=1` options to the command
  line.

  The first option is for using the terminal QEMU is running on as the Linux console.

  #pagebreak()

  === Guest network interfaces setup

  On the sender VM:

  ```sh
  ip addr add 10.0.0.2/24 dev enp0s2
  ```

  On the receiver VM:

  ```sh
  ip addr add 10.0.0.3/24 dev enp0s2
  ```

  == Compiling DPDK

  ```sh
  curl https://fast.dpdk.org/rel/dpdk-24.11.1.tar.xz -O
  tar -xf dpdk-24.11.1.tar.xz
  cd dpdk-stable-24.11.1/
  meson setup build -Dplatform=native
  cd build
  ninja
  sudo ninja install
  sudo sh -c 'echo /usr/local/lib > /etc/ld.so.conf.d/local.conf' # Needed on Arch Linux.
  sudo ldconfig
  ```

  == Compiling Pktgen-DPDK

  ```sh
  git clone https://github.com/pktgen/Pktgen-DPDK.git --branch pktgen-24.10.3
  cd Pktgen-DPDK
  PKG_CONFIG_PATH=/usr/local/lib/pkgconfig meson setup build
  cd build
  ninja
  sudo ninja install
  ```

  == Running Pktgen-DPDK

  Make sure to replace the PCI slot with your NIC's.

  ```sh
  sudo dpdk-devbind.py --bind vfio-pci 0000:00:02.0 --force
  sudo sh -c 'echo 512 > /sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages'
  sudo pktgen -l 0,1 -n 1 -a 0000:00:02.0 -- -P -T -m 1.0
  ```

  == Running the `AF_XDP` server

  I used an existing server implementation. It receives packets and drops
  them right away.

  ```sh
  git clone https://github.com/xdp-project/bpf-examples.git --recurse-submodules
  cd bpf-examples
  cd AF_XDP-example
  make
  ```

  #pagebreak()

  Run the _rxdrop_ example:

  ```sh
  sudo taskset -c 0 ./xdpsock \
    --interface enp0s2 \
    --busy-poll \
    --shared-umem \
    --rxdrop
  ```
]
