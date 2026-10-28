# GLib callback implemented in Tarn

Requires the GLib development library and the normal Tarn native toolchain.

```sh
tarn run examples/glib_callback/main.tarn --link glib-2.0
# 3
```

GLib stores a native adapter address and calls `tick` on the dispatching thread.
The count belongs to Tarn. Its raw pointer carries no loan: the unsafe caller
keeps the count alive and unmoved until `tick` returns 0 and GLib removes the
source. No handwritten C adapter, closure environment or callback registry is
needed. The native runtime and GLib remain dependencies; this is not a sandbox.

See [C interop](../../docs/ffi.md) for ABI and safety requirements.
