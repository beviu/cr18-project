#include <assert.h>
#include <chrono>
#include <liburing.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

#include <expected>
#include <optional>
#include <print>

struct owned_io_uring {
  io_uring ring = {
      .ring_fd = -1,
      .enter_ring_fd = -1,
  };

  static std::expected<owned_io_uring, int> initialize(unsigned int entries,
                                                       unsigned int flags) {
    io_uring ring;

    int ret = io_uring_queue_init(32, &ring, 0);
    if (ret < 0)
      return std::unexpected(-ret);

    return owned_io_uring{ring};
  }

  owned_io_uring(io_uring ring) : ring(ring) {}

  owned_io_uring(owned_io_uring &&other) : ring(other.ring) {
    other.ring.enter_ring_fd = -1;
  }

  owned_io_uring(const owned_io_uring &) = delete;

  ~owned_io_uring() {
    if (ring.enter_ring_fd != -1)
      io_uring_queue_exit(&ring);
  }
};

struct owned_fd {
  int fd = -1;

  static std::optional<owned_fd> create_socket(int domain, int type,
                                               int protocol) {
    int ret = socket(domain, type, protocol);
    if (ret == -1)
      return std::nullopt;

    return owned_fd{ret};
  }

  owned_fd(int fd) : fd(fd) {}

  owned_fd(owned_fd &&other) : fd(other.fd) { other.fd = -1; }

  owned_fd(const owned_fd &) = delete;

  ~owned_fd() {
    if (fd != -1)
      close(fd);
  }
};

int main() {
  auto queue = owned_io_uring::initialize(8, 0);
  if (!queue) {
    std::println(stderr, "io_uring_queue_init: {}", strerror(queue.error()));
    return EXIT_FAILURE;
  }

  const auto socket = owned_fd::create_socket(AF_INET, SOCK_DGRAM, 0);
  if (!socket) {
    perror("socket");
    return EXIT_FAILURE;
  }

  sockaddr_in addr = {};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  addr.sin_port = htons(12000);

  const auto start = std::chrono::steady_clock::now();

  unsigned long datagram_count = 0;

  for (;;) {
    const auto now = std::chrono::steady_clock::now();
    if (now - start > std::chrono::seconds(5))
      break;

    for (;;) {
      io_uring_sqe *sqe = io_uring_get_sqe(&queue->ring);
      if (!sqe)
        break;

      io_uring_prep_sendto(sqe, socket->fd, nullptr, 0, 0,
                           reinterpret_cast<const sockaddr *>(&addr),
                           sizeof(addr));
    }

    int ret = io_uring_submit(&queue->ring);
    if (ret < 0) {
      std::println(stderr, "io_uring_submit: {}", strerror(-ret));
      return EXIT_FAILURE;
    }

    unsigned int i = 0;

    unsigned int head;
    io_uring_cqe *cqe;
    io_uring_for_each_cqe(&queue->ring, head, cqe) {
      datagram_count++;
      i++;
    }

    io_uring_cq_advance(&queue->ring, i);
  }

  std::println(stdout, "Sent {} datagrams in 5 seconds.", datagram_count);

  return EXIT_SUCCESS;
}
