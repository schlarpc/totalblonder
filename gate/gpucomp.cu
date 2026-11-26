// gpucomp: a synthetic second CUDA context for the shared-GPU experiment (notes, optimization log
// experiment 40). It uses no video engine. Per period it launches one kernel (and/or one pinned
// host-to-device copy) and waits for it; it reports the round-trip time of each launch, which is
// how long a small piece of work of another process takes to get through next to the encoder.
//   gpucomp [--hz 60] [--iters 2000] [--blocks 1] [--copy BYTES] [--secs 10] [--idle] [--spin]
//   --hz 0: back to back (a context that always has work).  --idle: create the context, then sleep.
//   --iters: loop length per thread (256 threads per block); the kernel's duration scales with it.
#include <cuda_runtime.h>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>
#define CK(x) do { cudaError_t e_ = (x); if (e_ != cudaSuccess) { fprintf(stderr, "%s: %s\n", #x, cudaGetErrorString(e_)); exit(1); } } while (0)
static double now_s() { return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count(); }

__global__ void k_burn(unsigned *out, int iters) {
  unsigned x = blockIdx.x * 256u + threadIdx.x + 1u;
  for (int i = 0; i < iters; i++) x = x * 1664525u + 1013904223u + (x >> 7);
  out[blockIdx.x * 256 + threadIdx.x] = x;
}

int main(int argc, char **argv) {
  double hz = 60, secs = 10; int iters = 2000, blocks = 1; size_t copy = 0; bool idle = false, spin = false;
  for (int i = 1; i < argc; i++) {
    std::string a = argv[i];
    auto val = [&]() { return i + 1 < argc ? argv[++i] : (char *)"0"; };
    if (a == "--hz") hz = atof(val()); else if (a == "--secs") secs = atof(val());
    else if (a == "--iters") iters = atoi(val()); else if (a == "--blocks") blocks = atoi(val());
    else if (a == "--copy") copy = strtoull(val(), nullptr, 0); else if (a == "--idle") idle = true;
    else if (a == "--spin") spin = true;
    else { fprintf(stderr, "unknown option %s\n", a.c_str()); return 2; }
  }
  CK(cudaSetDeviceFlags(spin ? cudaDeviceScheduleSpin : cudaDeviceScheduleBlockingSync));
  unsigned *out; CK(cudaMalloc(&out, (size_t)blocks * 256 * sizeof(unsigned)));
  void *hbuf = nullptr, *dbuf = nullptr;
  if (copy) { CK(cudaMallocHost(&hbuf, copy)); memset(hbuf, 1, copy); CK(cudaMalloc(&dbuf, copy)); }
  k_burn<<<blocks, 256>>>(out, 10); CK(cudaDeviceSynchronize());
  double t0 = now_s();
  if (idle) { std::this_thread::sleep_for(std::chrono::duration<double>(secs)); printf("gpucomp idle context %.1f s\n", secs); return 0; }
  std::vector<double> rt;
  for (long n = 0;; n++) {
    double due = hz > 0 ? t0 + n / hz : now_s();
    double t = now_s();
    if (t - t0 >= secs) break;
    if (t < due) { std::this_thread::sleep_for(std::chrono::duration<double>(due - t)); t = now_s(); }
    if (iters > 0) k_burn<<<blocks, 256>>>(out, iters);
    if (copy) CK(cudaMemcpyAsync(dbuf, hbuf, copy, cudaMemcpyHostToDevice));
    CK(cudaDeviceSynchronize());
    rt.push_back(1e3 * (now_s() - t));
  }
  double wall = now_s() - t0, sum = 0;
  for (double x : rt) sum += x;
  std::sort(rt.begin(), rt.end());
  size_t k = rt.size();
  if (!k) return 0;
  printf("gpucomp hz %.0f iters %d blocks %d copy %zu: %zu launches in %.1f s, round trip ms min %.3f p10 %.3f p50 %.3f p90 %.3f p99 %.3f max %.3f mean %.3f, busy %.1f %%\n",
         hz, iters, blocks, copy, k, wall, rt[0], rt[k / 10], rt[k / 2], rt[k * 9 / 10], rt[std::min(k - 1, k * 99 / 100)], rt[k - 1], sum / k, 100 * sum / (1e3 * wall));
  return 0;
}
