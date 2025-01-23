project('io-uring-benchmark', 'cpp', default_options : ['cpp_std=c++23'])

liburing = dependency('liburing')

executable('io-uring-benchmark', 'src/main.cpp', dependencies : [liburing])
