// Host build of the encoder kernels and their launcher. See shim/cuda_runtime.h for the model.
#include <cuda_runtime.h>   // the shim (this file is compiled with -Ikernels/shim -D__CUDACC__)
#include <stdlib.h>
#include <ucontext.h>
#include <utility>

#include "kernels.h"
#include "pipe_kernels.h"
#include "tb_kernel_list.h"
#include "tb_kernels_ref.h"

thread_local tbk_dim3 threadIdx, blockIdx, blockDim, gridDim;

namespace {

// ---- kernels that test the launcher itself (tbk_test_launch); not part of the image. Each takes
// an output array of gridDim.x * blockDim.x words and one integer.
// t_sync: three phases through shared memory; out[t] = ((block << 16) | t) + 1 only if every
// thread of the block passed each barrier before any thread went on.
__global__ void t_sync(uint32_t *out, int) {
  __shared__ uint32_t s[1024];
  unsigned t = threadIdx.x, n = blockDim.x;
  s[t] = (blockIdx.x << 16) | t;
  __syncthreads();
  uint32_t a = s[(t + 1) % n];
  __syncthreads();
  s[t] = a + 1;
  __syncthreads();
  out[blockIdx.x * n + t] = s[(t + n - 1) % n];
}
// t_shfl_xor, t_shfl_up: the value of the partner lane of one shuffle with delta d.
__global__ void t_shfl_xor(uint32_t *out, int d) {
  uint32_t v = (blockIdx.x << 16) | threadIdx.x;
  out[blockIdx.x * blockDim.x + threadIdx.x] = __shfl_xor_sync(0xffffffffu, v, d);
}
__global__ void t_shfl_up(uint32_t *out, int d) {
  uint32_t v = (blockIdx.x << 16) | threadIdx.x;
  out[blockIdx.x * blockDim.x + threadIdx.x] = __shfl_up_sync(0xffffffffu, v, d);
}
// t_scan: an inclusive prefix sum over the block, as k_ent_rowsum does it (shuffles inside each
// warp, warp totals through shared memory), of the values t * k + 1.
__global__ void t_scan(uint32_t *out, int k) {
  __shared__ uint32_t ws[32];
  unsigned t = threadIdx.x, lane = t & 31, warp = t >> 5;
  uint32_t v = t * (uint32_t)k + 1, inc = v;
  for (int d = 1; d < 32; d <<= 1) {
    uint32_t u = __shfl_up_sync(0xffffffffu, inc, d);
    if (lane >= (unsigned)d) inc += u;
  }
  if (lane == 31 || t == blockDim.x - 1) ws[warp] = inc;
  __syncthreads();
  uint32_t base = 0;
  for (unsigned w = 0; w < warp; w++) base += ws[w];
  out[blockIdx.x * blockDim.x + t] = base + inc;
}
// Misuse, reported by the launcher: odd threads wait at __syncthreads while even threads
// shuffle (kind); thread 0 passes one barrier more than the others (count); the lanes shuffle
// with different deltas (delta).
__global__ void t_diverge_kind(uint32_t *out, int) {
  uint32_t v = threadIdx.x;
  if (threadIdx.x & 1) __syncthreads();
  else v = __shfl_xor_sync(0xffffffffu, v, 2);
  out[blockIdx.x * blockDim.x + threadIdx.x] = v;
}
__global__ void t_diverge_count(uint32_t *out, int) {
  if (threadIdx.x == 0) __syncthreads();
  __syncthreads();
  out[blockIdx.x * blockDim.x + threadIdx.x] = 1;
}
__global__ void t_diverge_delta(uint32_t *out, int) {
  uint32_t v = __shfl_xor_sync(0xffffffffu, threadIdx.x, (threadIdx.x & 1) ? 1 : 2);
  out[blockIdx.x * blockDim.x + threadIdx.x] = v;
}

// ---- calling a kernel from an array of pointers to its arguments
template <class... A, size_t... I>
void call_with(void (*f)(A...), void **p, std::index_sequence<I...>) {
  f(*static_cast<typename std::remove_reference<A>::type *>(p[I])...);
}
template <class... A> void call(void (*f)(A...), void **p) { call_with(f, p, std::index_sequence_for<A...>{}); }
template <class... A> int param_sizes(void (*)(A...), unsigned *sizes, int max) {
  const unsigned s[] = {(unsigned)sizeof(A)..., 0u};
  int n = (int)sizeof...(A);
  for (int i = 0; i < n && i < max; i++) sizes[i] = s[i];
  return n;
}

struct Entry {
  const char *name;
  int barriers;
  void (*run)(void **);
  int (*sizes)(unsigned *, int);
};
#define X(kname, kbarriers) \
  {#kname, kbarriers, [](void **p) { call(&kname, p); }, [](unsigned *s, int m) { return param_sizes(&kname, s, m); }},
const Entry k_entries[] = {TB_KERNELS(X)};
#undef X
const int k_count = (int)(sizeof k_entries / sizeof k_entries[0]);

// The launcher's own test kernels, in the order of tbk_test_launch's numbering. The last one is
// t_sync without the barrier flag: a kernel that reaches a barrier while it runs as a plain
// loop must be reported, not crash.
#define T(kname, kbarriers) \
  {#kname, kbarriers, [](void **p) { call(&kname, p); }, [](unsigned *s, int m) { return param_sizes(&kname, s, m); }},
const Entry t_entries[] = {T(t_sync, 1) T(t_shfl_xor, 1) T(t_shfl_up, 1) T(t_scan, 1) T(t_diverge_kind, 1)
                               T(t_diverge_count, 1) T(t_diverge_delta, 1) T(t_sync, 0)};
#undef T
const int t_count = (int)(sizeof t_entries / sizeof t_entries[0]);

// ---- fibers: the threads of one block of a kernel with barriers
enum { MAX_BLOCK = 1024, STACK_BYTES = 256 * 1024 };
enum State { RUNNING, AT_SYNC, AT_SHUFFLE, DONE };

struct Fibers {
  ucontext_t sched;
  ucontext_t ctx[MAX_BLOCK];
  State state[MAX_BLOCK];
  int kind[MAX_BLOCK];        // the shuffle a thread waits in: kind and delta
  int delta[MAX_BLOCK];
  uint64_t pub[MAX_BLOCK];    // values published at the shuffle the threads are waiting in
  uint64_t seen[MAX_BLOCK];   // the same, as the resumed threads read them
  char *stacks;
  unsigned cur;
  const Entry *entry;
  void **params;
};
thread_local Fibers *t_f = nullptr;
// Set while the threads of a block run as fibers. A barrier reached outside (a kernel run as a
// plain loop although it has barriers: a wrong flag in tb_kernel_list.h) sets t_misuse instead.
thread_local bool t_in_fibers = false;
thread_local bool t_misuse = false;

void fiber_main() {
  Fibers *f = t_f;
  f->entry->run(f->params);
  f->state[f->cur] = DONE;
  swapcontext(&f->ctx[f->cur], &f->sched);   // never resumed
}

// Returns 0, -3 (barriers not reached together) or -4.
int run_block_fibers(const Entry &e, unsigned block, void **params) {
  if (!t_f) {
    t_f = static_cast<Fibers *>(calloc(1, sizeof(Fibers)));   // plain data: no constructor to run
    if (!t_f) return -4;
  }
  Fibers *f = t_f;
  if (!f->stacks) {
    f->stacks = static_cast<char *>(malloc((size_t)MAX_BLOCK * STACK_BYTES));
    if (!f->stacks) return -4;
  }
  f->entry = &e;
  f->params = params;
  for (unsigned t = 0; t < block; t++) {
    getcontext(&f->ctx[t]);
    f->ctx[t].uc_stack.ss_sp = f->stacks + (size_t)t * STACK_BYTES;
    f->ctx[t].uc_stack.ss_size = STACK_BYTES;
    f->ctx[t].uc_link = &f->sched;
    makecontext(&f->ctx[t], fiber_main, 0);
    f->state[t] = RUNNING;
    f->kind[t] = f->delta[t] = 0;
    f->pub[t] = f->seen[t] = 0;
  }
  t_in_fibers = true;
  int r = 0;
  for (;;) {
    // One step: every live thread runs to its next barrier (or to its end).
    unsigned done = 0, at_sync = 0, at_shuffle = 0;
    bool same_shuffle = true;
    int kind = -1, delta = 0;
    for (unsigned t = 0; t < block; t++) {
      if (f->state[t] == DONE) { done++; continue; }
      f->cur = t;
      threadIdx.x = t;
      f->state[t] = RUNNING;
      swapcontext(&f->sched, &f->ctx[t]);
      if (f->state[t] == DONE) done++;
      else if (f->state[t] == AT_SYNC) at_sync++;
      else {
        at_shuffle++;
        if (kind < 0) { kind = f->kind[t]; delta = f->delta[t]; }
        else if (f->kind[t] != kind || f->delta[t] != delta) same_shuffle = false;
      }
    }
    if (done == block) break;
    // CUDA requires all threads of a block at a __syncthreads and all lanes of a warp at a
    // full-mask shuffle. The kernels here go further: every thread executes the same sequence
    // of barriers, so that in each step all threads stop at the same kind of barrier (the same
    // shuffle) or all end. That is what this lockstep needs; anything else is reported.
    if (done || (at_sync && at_shuffle) || !same_shuffle) { r = -3; break; }
    for (unsigned t = 0; t < block; t++) f->seen[t] = f->state[t] == AT_SHUFFLE ? f->pub[t] : 0;
  }
  t_in_fibers = false;
  return r;
}

int launch_entry(const Entry &e, unsigned grid, unsigned block, void **params) {
  if (block < 1 || block > MAX_BLOCK) return -2;
  gridDim = {grid, 1, 1};
  blockDim = {block, 1, 1};
  blockIdx = {0, 0, 0};
  threadIdx = {0, 0, 0};
  t_misuse = false;
  for (unsigned b = 0; b < grid; b++) {
    blockIdx.x = b;
    if (e.barriers) {
      int r = run_block_fibers(e, block, params);
      if (r) return r;
    } else {
      for (unsigned t = 0; t < block; t++) {
        threadIdx.x = t;
        e.run(params);
      }
      if (t_misuse) return -5;
    }
  }
  return 0;
}

}  // namespace

void tbk_barrier() {
  if (!t_in_fibers) { t_misuse = true; return; }
  Fibers *f = t_f;
  f->state[f->cur] = AT_SYNC;
  swapcontext(&f->ctx[f->cur], &f->sched);
}

uint64_t tbk_shuffle(uint64_t value, int kind, int d) {
  if (!t_in_fibers) { t_misuse = true; return value; }
  Fibers *f = t_f;
  unsigned t = f->cur, lane = t & 31;
  f->pub[t] = value;
  f->kind[t] = kind;
  f->delta[t] = d;
  f->state[t] = AT_SHUFFLE;
  swapcontext(&f->ctx[t], &f->sched);
  if (kind == 0) {   // xor: the partner is in the same warp for d < 32
    unsigned p = t ^ (unsigned)d;
    return p < blockDim.x ? f->seen[p] : 0;
  }
  return lane >= (unsigned)d ? f->seen[t - (unsigned)d] : value;   // up
}

extern "C" int tbk_kernel_count(void) { return k_count; }
extern "C" const char *tbk_kernel_name(int k) { return k >= 0 && k < k_count ? k_entries[k].name : nullptr; }
extern "C" int tbk_kernel_params(int k, unsigned *sizes, int max) {
  return k >= 0 && k < k_count ? k_entries[k].sizes(sizes, max) : -1;
}

extern "C" int tbk_launch(int k, unsigned grid, unsigned block, void **params) {
  if (k < 0 || k >= k_count) return -1;
  return launch_entry(k_entries[k], grid, block, params);
}

extern "C" int tbk_sanitized(void) {
#ifdef __SANITIZE_ADDRESS__
  return 1;
#else
  return 0;
#endif
}

extern "C" int tbk_test_kernel_count(void) { return t_count; }

extern "C" int tbk_test_launch(int k, unsigned grid, unsigned block, uint32_t *out, int arg) {
  if (k < 0 || k >= t_count) return -1;
  void *params[2] = {&out, &arg};
  return launch_entry(t_entries[k], grid, block, params);
}
