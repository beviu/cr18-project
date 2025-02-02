#import "@preview/touying:0.5.5": *
#import themes.simple: simple-theme, title-slide

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

#columns(2, [
    #text(13pt, [
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
    ])

    - *readiness-based*
    - one system call per operation
    - *non-blocking*
    - variants: `poll`, *`epoll`*, `kqueue`
])

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

= DPDK



= Questions
