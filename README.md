# CR18 project

## Ethernet

### Generating packets

I used the `pktgen` kernel module to generate packets.

```console
$ git clone https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git
$ cd samples/pktgen
$ ./pktgen_sample01_simple.sh -i <interface> -s <packet_size> -m <mac_address>
```

### DPDK



### `AF_XDP`

I used an existing server implementation. It receives packets and drops
them right away.

```console
$ git clone https://github.com/xdp-project/bpf-examples.git --recurse-submodules
$ cd bpf-examples
```

On NixOS, I had to disable `-Wunused-command-line-argument` for BPF
programs to compile because the clang wrapper unconditionally prepends
the `--gcc-toolchain` flag and that flag is not used when targetting BPF
with `-target bpf`.

```console
$ sed '/-Werror/d' -i lib/xdp-tools/lib/libxdp/Makefile
```

Then build and run the rxdrop example:

```
$ cd AF_XDP-example
$ make
$ sudo taskset -c <app core> ./xdpsock -i <interface> -q <queue_id> -r
```
