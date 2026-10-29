# ADR 0091: native libraries in the package manifest

Status: accepted and implemented.

## Context

ADR 0047 made linking an explicit grant through `tarn build/run --link
<name>` and called the flag "a temporary interface until `tarn.toml`
exists". The manifest now exists. Every build script of a native package
repeated the same flags. The tarn-ui GTK port found it was missing `gio-2.0`
only from a raw linker error naming `g_list_model_get_n_items`.

## Decision

- `tarn.toml` accepts `[native] links = [...]`. Entries use the `--link`
  spelling (`gtk-4`, `:libfoo.so.1`) and pass the same validation. Duplicates
  are removed, and any other key under `[native]` is rejected, so the
  manifest cannot carry linker flags or paths.
- `build`, `run`, `test` and `profile` link the entry package's `links`
  first, then the `--link` flags that are not already listed.
- Only the entry package grants links. A dependency cannot make the build
  link a library. Its `[native]` table is ignored, and the root package must
  list what the dependency needs. This keeps ADR 0047's rule that declarations
  and imports are not grants.
- When symbols are undefined, the linker message suggests the library that
  usually defines them, using a small table of well-known prefixes (GTK,
  GIO, GObject, GLib, Pango, Cairo, SQLite, curl, OpenSSL). It is only a
  hint. Nothing is linked implicitly.

## Consequences

- A native package states its libraries once, next to its name and entry.
- `--link` stays for one-off builds and for files outside a package.
- Dependencies that need C libraries must document them. A way to request
  links that the root approves can come later, alongside `deps --trust`.
