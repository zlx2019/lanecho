// Architecture-gated compilation unit for NEON (lanecho-owned, not vendored):
// SPM cannot select source files by target architecture, while blake3_neon.c
// unconditionally includes <arm_neon.h> and cannot compile on x86_64. This
// file gates the inclusion during preprocessing and leaves the vendored file
// unchanged. (blake3_neon.c is excluded in Package.swift and is not compiled
// directly.)
#if defined(__aarch64__)
#include "blake3_neon.c"
#endif
