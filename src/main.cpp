#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <expected>
#include <list>
#include <memory>
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

template <size_t BufferSize> class naive_buffer_allocator {
public:
  typedef std::array<unsigned char, BufferSize> buffer;

  buffer *get_buffer() const { return new (std::nothrow) buffer; }

  void release_buffer(buffer *buf) const { delete buf; }
};

template <size_t BufferSize> class buffer_pool {
public:
  typedef std::array<unsigned char, BufferSize> buffer;

  static std::optional<buffer_pool<BufferSize>>
  create_with_buffer_count(size_t buf_count) {
    buffer_pool<BufferSize> pool;

    for (size_t i = 0; i < buf_count; ++i) {
      const auto buf = new (std::nothrow) buffer;
      if (!buf)
        return std::nullopt;

      pool.free_buffers.emplace_front(buf);
    }

    return pool;
  }

  buffer *get_buffer() {
    if (free_buffers.empty())
      return nullptr;

    auto buf = std::move(free_buffers.front());
    free_buffers.pop_front();

    return buf.release();
  }

  void release_buffer(buffer *buf) { free_buffers.emplace_back(buf); }

private:
  std::list<std::unique_ptr<buffer>> free_buffers;
};

static void fill_buffer(std::span<unsigned char> buf) {
  for (size_t i = 0; i < buf.size(); ++i)
    buf[i] = 'A' + i % 26;
}

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
      auto buf = buffers.get_buffer();
      if (!buf) {
        std::println(stderr, "Not enough buffers.");
        break;
      }

      io_uring_sqe *sqe = io_uring_get_sqe(&queue->ring);
      if (!sqe) {
        buffers.release_buffer(buf);
        break;
      }

      fill_buffer(*buf);

      if (fixed_files) {
        io_uring_prep_sendto(sqe, 0, buf->data(), buf->size(), 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
        sqe->flags |= IOSQE_FIXED_FILE;
      } else {
        io_uring_prep_sendto(sqe, socket->fd, buf->data(), buf->size(), 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
      }

      io_uring_sqe_set_data(sqe, buf);
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
      buffers.release_buffer(
          reinterpret_cast<BufferAllocator::buffer *>(cqe->user_data));

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
  constexpr const size_t buffer_size = 16;

  naive_buffer_allocator<buffer_size> naive_buf_alloc;

  auto buf_pool = buffer_pool<buffer_size>::create_with_buffer_count(256);
  if (!buf_pool) {
    std::println(stderr, "Failed to create buffer pool.");
    return EXIT_FAILURE;
  }

  if (!run(false, naive_buf_alloc, "basic"))
    return EXIT_FAILURE;

  if (!run(false, *buf_pool, "buffer pool"))
    return EXIT_FAILURE;

  if (!run(true, *buf_pool, "fixed files"))
    return EXIT_FAILURE;

  return EXIT_SUCCESS;
}
