#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <expected>
#include <optional>
#include <print>
#include <span>
#include <string_view>

#include <liburing.h>
#include <netinet/in.h>
#include <sys/socket.h>

struct owned_io_uring {
  io_uring ring = {
      .ring_fd = -1,
      .enter_ring_fd = -1,
  };

  static std::expected<owned_io_uring, int> initialize(unsigned int entries,
                                                       unsigned int flags) {
    io_uring ring;

    const int ret = io_uring_queue_init(32, &ring, 0);
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

class owned_io_uring_buf_ring {
public:
  static std::expected<owned_io_uring_buf_ring, int>
  setup(io_uring *ring, unsigned int entry_count, int buffer_group_id,
        unsigned int flags) {
    int err;

    io_uring_buf_ring *buf_ring = io_uring_setup_buf_ring(
        ring, entry_count, buffer_group_id, flags, &err);
    if (!ring)
      return std::unexpected(-err);

    return owned_io_uring_buf_ring{ring, buf_ring, entry_count,
                                   buffer_group_id};
  }

  owned_io_uring_buf_ring(io_uring *ring, io_uring_buf_ring *buf_ring,
                          unsigned int entry_count, int buffer_group_id)
      : buf_ring(buf_ring), entry_count_(entry_count),
        buffer_group_id_(buffer_group_id) {}

  owned_io_uring_buf_ring(owned_io_uring_buf_ring &&other)
      : ring_(other.ring_) {
    other.ring_ = nullptr;
  }

  owned_io_uring_buf_ring(const owned_io_uring_buf_ring &) = delete;

  ~owned_io_uring_buf_ring() {
    if (ring_ != nullptr)
      io_uring_free_buf_ring(ring_, buf_ring, entry_count_, buffer_group_id_);
  }

  io_uring_buf_ring *operator*() const { return buf_ring; }
  io_uring *ring() const { return ring_; }
  unsigned int entry_count() const { return entry_count_; }
  int buffer_group_id() const { return buffer_group_id_; }

private:
  io_uring *ring_;
  io_uring_buf_ring *buf_ring = nullptr;
  unsigned int entry_count_;
  int buffer_group_id_;
};

struct owned_fd {
  int fd = -1;

  static std::optional<owned_fd> create_socket(int domain, int type,
                                               int protocol) {
    const int ret = socket(domain, type, protocol);
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

class naive_buffer_allocator {
public:
  naive_buffer_allocator(size_t buffer_size) : buffer_size(buffer_size) {}

  std::span<unsigned char> get_buffer() const {
    const auto buf = new (std::nothrow) unsigned char[buffer_size];
    return {buf, buffer_size};
  }

  void release_buffer(unsigned char *buf) const { delete[] buf; }

private:
  size_t buffer_size;
};

template <size_t Capacity> class buffer_pool {
public:
  buffer_pool(size_t buffer_size) : buffer_size(buffer_size) {}

  buffer_pool(buffer_pool &&other)
      : buffer_size(other.buffer_size), ring(other.ring), head(other.head),
        tail(other.tail) {
    other.head = other.tail;
  }

  buffer_pool(const buffer_pool &) = delete;

  ~buffer_pool() {
    while (head != tail)
      delete[] ring[head++ % Capacity];
  }

  std::span<unsigned char> get_buffer() {
    if (head == tail)
      return {};

    return {ring[head++ % Capacity], buffer_size};
  }

  bool release_buffer(unsigned char *buf) {
    if (tail == head + Capacity)
      return false;

    ring[tail++ % Capacity] = buf;

    return true;
  }

  bool add_new_buffer() {
    if (tail == head + Capacity)
      return false;

    const auto buf = new (std::nothrow) unsigned char[buffer_size];
    if (!buf)
      return false;

    ring[tail++ % Capacity] = buf;

    return true;
  }

  bool reserve(size_t count) {
    for (size_t i = 0; i < count; ++i) {
      if (!add_new_buffer())
        return false;
    }

    return true;
  }

private:
  size_t buffer_size;

  std::array<unsigned char *, Capacity> ring;

  size_t head = 0;
  size_t tail = 0;
};

template <class BufferAllocator>
static bool run(bool fixed_files, BufferAllocator &buffers,
                std::string_view name) {
  auto queue = owned_io_uring::initialize(8, 0);
  if (!queue) {
    std::println(stderr, "io_uring_queue_init: {}", strerror(queue.error()));
    return false;
  }

  const auto socket = owned_fd::create_socket(AF_INET, SOCK_DGRAM, 0);
  if (!socket) {
    perror("socket");
    return false;
  }

  if (fixed_files) {
    const int ret = io_uring_register_files(&queue->ring, &socket->fd, 1);
    if (ret != 0) {
      std::println(stderr, "io_uring_register_files: {}", strerror(ret));
      return false;
    }
  }

  sockaddr_in addr = {};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  addr.sin_port = htons(12000);

  const auto start = std::chrono::steady_clock::now();

  unsigned long datagram_count = 0;

  for (;;) {
    const auto now = std::chrono::steady_clock::now();
    if (now - start > std::chrono::seconds(1))
      break;

    for (;;) {
      const auto buf = buffers.get_buffer();
      if (!buf.data()) {
        std::println(stderr, "Not enough buffers.");
        break;
      }

      io_uring_sqe *sqe = io_uring_get_sqe(&queue->ring);
      if (!sqe) {
        buffers.release_buffer(buf.data());
        break;
      }

      if (fixed_files) {
        io_uring_prep_sendto(sqe, 0, buf.data(), buf.size(), 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
        sqe->flags |= IOSQE_FIXED_FILE;
      } else {
        io_uring_prep_sendto(sqe, socket->fd, buf.data(), buf.size(), 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
      }

      io_uring_sqe_set_data(sqe, buf.data());
    }

    int ret = io_uring_submit(&queue->ring);
    if (ret < 0) {
      std::println(stderr, "io_uring_submit: {}", strerror(-ret));
      return false;
    }

    unsigned int i = 0;

    unsigned int head;
    io_uring_cqe *cqe;
    io_uring_for_each_cqe(&queue->ring, head, cqe) {
      buffers.release_buffer(reinterpret_cast<unsigned char *>(cqe->user_data));

      if (cqe->res < 0) {
        std::println(stderr, "sendto: {}", strerror(-cqe->res));
        return false;
      }

      datagram_count++;
      i++;
    }

    io_uring_cq_advance(&queue->ring, i);
  }

  std::println(stdout, "{}: {}", name, datagram_count);

  return true;
}

int main() {
  constexpr const size_t buf_size = 16;
  constexpr const size_t buf_count = 256;

  naive_buffer_allocator naive_buf_alloc(buf_size);

  buffer_pool<buf_count> buf_pool(buf_size);
  if (!buf_pool.reserve(buf_count)) {
    std::println(stderr, "Failed to create buffer pool.");
    return EXIT_FAILURE;
  }

  if (!run(false, naive_buf_alloc, "basic"))
    return EXIT_FAILURE;

  if (!run(false, buf_pool, "buffer pool"))
    return EXIT_FAILURE;

  if (!run(true, buf_pool, "fixed files"))
    return EXIT_FAILURE;

  return EXIT_SUCCESS;
}
