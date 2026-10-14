# Personal patch: GPU-load lag fix

Temporary local patch until upstream fixes it. Not intended for upstreaming as-is.

## Symptom

Since `11a5c830` ("rework WGfx to work with ash instead of vulkano"), WayVR becomes very
laggy when the GPU is at full load (e.g. a game running), while the game itself stays smooth.
`0eab3907` and earlier were fine.

## Cause

The vulkano version submitted all overlay command buffers for a frame together and waited
on the CPU once; one-off uploads (`build_and_execute_now`) were flushed without waiting.

The ash rework made every submission blocking (`submit_and_wait`):

- `GpuFutures::wait` submitted and waited on each command buffer one by one
- `build_and_execute_now` waited for every upload
- OpenVR backend: `transition_image_now` added an extra blocking submit per dirty overlay

An idle GPU completes each wait almost instantly. A GPU saturated by another process
time-slices between contexts, so each CPU round-trip can cost a full time slice, multiplied
by the number of submissions per frame.

## Fix

- `wgui/src/gfx/raw.rs`: every command buffer begins with an ALL_COMMANDS -> ALL_COMMANDS
  memory barrier (`record_queue_completion_barrier`), so queue submission order alone
  orders it after all earlier work (same guarantee the CPU waits used to provide).
  Added non-blocking `RawCommandBuffer::submit()` returning a `RawFence`.
- `wgui/src/gfx/mod.rs`: added `WGfx::submit_all_and_wait` (submit everything, wait once)
  and a private `submit_async` that keeps command buffers + resources alive in a pending
  list until their fence signals. `transition_image_now` now uses it.
- `wgui/src/gfx/cmd.rs`: `build_and_execute` / `build_and_execute_now` use `submit_async`.
  Work on the dedicated capture queue still blocks (cross-queue ordering relies on the
  CPU wait, and it runs on the capture thread anyway).
- `wayvr/src/graphics/mod.rs`: `GpuFutures::wait(&gfx)` uses `submit_all_and_wait`
  (one CPU wait per frame, like before the rework). Call sites in the openvr/openxr
  backends updated.

Changed code is marked with `HACK(fix-gpu-lag)`.

## Trade-offs / if issues remain

- The barrier at the start of every command buffer over-synchronizes on the GPU; the cost
  should be negligible compared to the removed CPU stalls.
- A command pool is still created/destroyed per command buffer (CPU overhead, not
  GPU-load dependent) — next suspect if lag remains.
