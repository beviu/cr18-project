#import "@preview/touying:0.5.5": *
#import themes.simple: simple-theme, title-slide
#import "@preview/cetz:0.3.2"

#show: simple-theme.with(
  primary: fuchsia,
  author: [Greg Depoire-\-Ferrer],
)

#title-slide[
  = Networking performance with io_uring
]

= Different APIs

== BSD socket API

- Create a socket: `socket`, `accept`
- Receiving data: `recv`, `recvfrom`, `read`
- Sending data: `send`, `sendto`, `write`

== Example

```python
import random
import socket

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(('0.0.0.0', 53))

while True:
    datagram, address = s.recvfrom(2048)
    print(f'Received {datagram} from {address}.')
```

== Characteristics

- one system call per operation
- *blocking*: no compute or IO can be done in the meanwhile

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

    - *readiness-based*
    - one system call per operation
    - *non-blocking*
    - variants: `poll`, *`epoll`*, `kqueue`
  ],
)

== Readiness-based interface

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

== Completion-based interface (fake API)

```c
char *buf = malloc(2048);
start_read_operation(s, buf, sizeof(buf), 1234);

/* Later... */

int id = process_completed_operation();
printf("%d\n", id); // 1234
```

= `io_uring`

== Presentation

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
        line((outer-radius + 0.6, 0), (outer-radius + 0.1, 0), mark: (end: ">"))
      })
    }
  })
}

#align(center + horizon, image("io_uring.png", height: 1fr))

== Features

- fixed files and buffers (May 2019)
- buffer ring (July 2022)
- zero-copy transmission (October 2022)
- zero-copy reception (not merged yet!)

== Fixed files

To reduce the overhead of:
- reference counting
- descriptor table lookup

```c
int io_uring_register_files(
    struct io_uring *ring,
    const int *files,
    unsigned nr_files,
);
```

== Buffer ring

To reduce the overhead of:
- bound checks
- locking memory in RAM

```c
struct io_uring_buf_ring *io_uring_setup_buf_ring(
    struct io_uring *ring,
    unsigned int nentries,
    int bgid,
    unsigned int flags,
    int *ret,
);
```

== Zero-copy transmission

To reduce the overhead of:
- copying application memory to kernel memory

```c
void io_uring_prep_send_zc(
    struct io_uring_sqe *sqe,
    int sockfd,
    const void *buf,
    size_t len,
    int flags,
    unsigned zc_flags,
);
```

== Zero-copy reception

To reduce the overhead of:
- copying kernel memory to application memory

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

#cetz.canvas({
  import cetz.draw: *

  column((0, 0), (3, 4), (silver, silver, silver, silver, silver, silver))
  content((1.5, -1), [Area], anchor: "north")

  ring(
    (6, 2),
    0.75,
    1.25,
    (
      blue,
      blue,
      blue,
      silver,
      silver,
      silver,
      silver,
      silver,
      silver,
    ),
    cursors: (
      ([head], 0),
      ([tail], 3),
    ),
  )
  content((6, -1), align(center, [Refill \ buffer]), anchor: "north")
})

+ The application submits a `RECV_ZC` operation.
+ The kernel picks a free buffer in the area.
+ The NIC writes to the buffer.
+ The kernel enqueues a completion queue entry.
+ The application processes data in the buffer.
+ The application enqueues a refill queue entry with the buffer to give ownership back to the kernel.

= Benchmarks

