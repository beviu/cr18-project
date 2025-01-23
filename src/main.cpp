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

class buffer_allocator {
public:
  virtual ~buffer_allocator() = default;

  virtual std::unique_ptr<char[]> get_buffer() = 0;
  virtual void release_buffer(std::unique_ptr<char[]>) = 0;
};

class naive_buffer_allocator : public buffer_allocator {
public:
  naive_buffer_allocator(size_t buffer_size) : buffer_size(buffer_size) {}

  std::unique_ptr<char[]> get_buffer() override {
    char *buffer = new (std::nothrow) char[buffer_size];
    return std::unique_ptr<char[]>(buffer);
  }

  void release_buffer(std::unique_ptr<char[]> buffer) override {}

private:
  size_t buffer_size;
};

class buffer_pool : public buffer_allocator {
public:
  buffer_pool(size_t buffer_size, size_t buffer_count) {
    for (size_t i = 0; i < buffer_count; ++i)
      free_buffers.emplace_back(new char[buffer_size]);
  }

  std::unique_ptr<char[]> get_buffer() override {
    if (free_buffers.empty())
      return nullptr;

    auto buffer = std::move(free_buffers.front());
    free_buffers.pop_front();

    return buffer;
  }

  void release_buffer(std::unique_ptr<char[]> buffer) override {
    free_buffers.emplace_back(std::move(buffer));
  }

private:
  std::list<std::unique_ptr<char[]>> free_buffers;
};

static void fill_buffer(std::span<char> buffer) {
  for (char &c : buffer)
    c = 'A' + (rand() % 26);
}

static bool run(bool fixed_files, bool use_buffer_pool, std::string_view name) {
  auto queue = owned_io_uring::initialize(8, 0);
  if (!queue) {
    std::println(stderr, "io_uring_queue_init: {}", strerror(queue.error()));
    return false;
  }

  constexpr const size_t buffer_size = 16;

  std::unique_ptr<buffer_allocator> buffers;

  if (use_buffer_pool) {
    buffers =
        std::make_unique<buffer_pool>(buffer_size, queue->ring.sq.ring_sz);
  } else {
    buffers = std::make_unique<naive_buffer_allocator>(buffer_size);
  }

  const auto socket = owned_fd::create_socket(AF_INET, SOCK_DGRAM, 0);
  if (!socket) {
    perror("socket");
    return false;
  }

  if (fixed_files) {
    int ret = io_uring_register_files(&queue->ring, &socket->fd, 1);
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
      io_uring_sqe *sqe = io_uring_get_sqe(&queue->ring);
      if (!sqe)
        break;

      auto buffer = buffers->get_buffer();
      fill_buffer({buffer.get(), buffer_size});

      if (fixed_files) {
        io_uring_prep_sendto(sqe, 0, buffer.get(), buffer_size, 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
        sqe->flags |= IOSQE_FIXED_FILE;
      } else {
        io_uring_prep_sendto(sqe, socket->fd, buffer.get(), buffer_size, 0,
                             reinterpret_cast<const sockaddr *>(&addr),
                             sizeof(addr));
      }

      sqe->user_data = reinterpret_cast<uint64_t>(buffer.release());
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
      std::unique_ptr<char[]> buffer(reinterpret_cast<char *>(cqe->user_data));
      buffers->release_buffer(std::move(buffer));

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
  if (!run(false, false, "basic"))
    return EXIT_FAILURE;

  if (!run(false, true, "buffer pool"))
    return EXIT_FAILURE;

  if (!run(true, true, "fixed files"))
    return EXIT_FAILURE;

  return EXIT_SUCCESS;
}
