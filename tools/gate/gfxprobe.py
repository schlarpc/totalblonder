#!/usr/bin/env python3
"""gfxprobe.py SECONDS [HZ [WORK]] : what a desktop frame waits for on this card.

For a card that also serves a desktop (bagel's). Draws one full-screen 2560x1440 frame into an
offscreen target at HZ (default 60) on the graphics queue of the default GPU (EGL, no window)
and waits for it (glFinish). WORK adds that many sine pairs per pixel: 0 is a trivial frame
(0.4 ms on an idle RX 7900 XTX), 800 one of about 3 ms. Prints one line per second with the
median and the worst frame time, and a summary: median, p99, max, and how many frames took
longer than 4 / 8 / 16.7 ms. Whatever a compute user adds to these is what the compositor's
frames wait; the first frame of a run pays the context's start-up (5 to 10 ms) and is left out
of the summary. Needs moderngl and libEGL (nix run .#gate-gfxprobe); the capacity bench runs it
beside every run when M2V_GFXPROBE names it (tools/gate/bench.py).
"""
import signal
import statistics
import struct
import sys
import time

import moderngl

stop = []
for sig in (signal.SIGINT, signal.SIGTERM):   # a caller ends the probe early and still gets the summary
    signal.signal(sig, lambda *a: stop.append(1))
secs = float(sys.argv[1]); hz = float(sys.argv[2]) if len(sys.argv) > 2 else 60.0
work = int(sys.argv[3]) if len(sys.argv) > 3 else 0   # extra shading per pixel: makes the frame heavier
ctx = moderngl.create_standalone_context(backend="egl")
prog = ctx.program(
    vertex_shader="#version 330\nin vec2 p; out vec2 uv; void main(){uv=p; gl_Position=vec4(p,0,1);}",
    fragment_shader="#version 330\nin vec2 uv; out vec4 c; uniform float t; uniform int work; void main(){float a=0.0; for(int i=0;i<work;i++) a+=sin(uv.x*float(i)+t)*cos(uv.y*float(i)-t); c=vec4(a*1e-9+0.5+0.5*sin(uv.x*40.0+t),0.5+0.5*cos(uv.y*40.0-t),0.5,1);}")
vbo = ctx.buffer(struct.pack("8f", -1, -1, 1, -1, -1, 1, 1, 1))
vao = ctx.simple_vertex_array(prog, vbo, "p")
fbo = ctx.simple_framebuffer((2560, 1440))
fbo.use()
print("renderer:", ctx.info["GL_RENDERER"], flush=True)
all_ms = []; t0 = time.perf_counter(); k = 0; sec = []; nsec = 1
while True:
    due = t0 + k / hz
    now = time.perf_counter()
    if now < due: time.sleep(due - now)
    if due - t0 >= secs or stop: break
    a = time.perf_counter()
    prog["t"].value = k * 0.1; prog["work"].value = work
    vao.render(moderngl.TRIANGLE_STRIP)
    ctx.finish()
    ms = (time.perf_counter() - a) * 1e3
    all_ms.append(ms); sec.append(ms); k += 1
    if due - t0 >= nsec:
        print(f"t={nsec:3d}s  median {statistics.median(sec):6.2f}  max {max(sec):7.2f} ms", flush=True); sec = []; nsec += 1
s = sorted(all_ms[1:])
print(f"frames {len(s)}  median {s[len(s)//2]:.2f}  p99 {s[int(len(s)*0.99)]:.2f}  max {s[-1]:.2f} ms  >4ms {sum(x>4 for x in s)}  >8ms {sum(x>8 for x in s)}  >16.7ms {sum(x>16.7 for x in s)}")
