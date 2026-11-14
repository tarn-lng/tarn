# Freestanding programs: symbols, sections, statics, linking

Design: [ADR 0101](adr/0101-freestanding-kernel-target.md). Everything here
works on every target; sections need an ELF object (Linux, `*-none`), and a
linked image is built only for `*-none`.

A kernel, bootloader or firmware image needs three things a hosted program
never asks for: names the boot code can find, data at a fixed layout in a
named section, and a link step driven by a linker script. Tarn adds
**attributes** (`#[name(argument)]` on the line before a function or static)
and **`static` items**.

```tarn
import "ffi"

// A boot-loader request: C layout, in the section the loader scans.
struct Request {
    id [4]u64
    revision u64
    response *mut u64
}

#[section(".limine_requests")]
#[align(8)]
static mut MEMORY_MAP Request = Request{
    id: [4]u64{0xc7b1dd30df4c8b88, 0x0a82e883a194f07b, 0x9d5827dcd881dd75, 0xa3148604b1f4faa0},
    revision: 0,
    response: ffi.null(),
}

#[align(16)]
static mut BOOT_STACK [16384]u8

// The freestanding contract, written in Tarn: no undefined symbols are left.
#[export("tarn_rt_fault")]
fn fault() {
    for true {}
}

#[export("tarn_rt_panic")]
fn panic_message(message *u8) {
    for true {}
}

#[export("tarn_rt_panic_at")]
fn panic_at(message *u8, location *u8) {
    for true {}
}

#[section(".text.boot")]
#[export("kmain")]
fn kmain() {
    for true {}
}
```

```sh
tarn build kernel.tarn --target x86_64-none --linker-script kernel.ld -o kernel.elf
```

## Attributes

| Attribute | On | Meaning |
|---|---|---|
| `#[export("name")]` | function, static | A global symbol `name` (a C identifier) with the C ABI. For a function it is an adapter to the Tarn body, so the body keeps Tarn's checks. |
| `#[section(".name")]` | function, static | Place the code or data in the section `.name` (`.` then letters, digits, `.`, `_`, `-`). ELF only. |
| `#[align(n)]` | static | At least `n`-byte alignment; a power of two up to 65536. |

Rules, all reported as E3086 (E1023 for a malformed attribute):

- An exported function is a plain function: not generic, not `async`, not a
  method, not `extern`. Parameters and result are integers, floats, `bool` or
  raw pointers (the same set as `extern "C"`); the result may also be `void`.
- A symbol is exported once. Names beginning `tarn_` belong to the compiler,
  except that a `*-none` program may define the freestanding contract
  (`tarn_rt_fault`, `tarn_rt_panic`, `tarn_rt_panic_at`, `tarn_alloc`,
  `tarn_free`, `tarn_realloc`) and must then match the contract's signature:
  `tarn_rt_fault()`, `tarn_rt_panic(*u8)`, `tarn_rt_panic_at(*u8, *u8)`,
  `tarn_alloc(usize) *mut u8`, `tarn_free(*mut u8)`,
  `tarn_realloc(*mut u8, usize) *mut u8`. `main` is reserved on hosted
  targets.
- An exported function is a root of the program even if nothing calls it. On
  `*-none` a program with an exported function does not need `fn main`; the
  `tarn_main` shim is emitted only when `main` exists.
- `#[section]` on a function puts the exported adapter (if any) and the Tarn
  body in that section; every other function goes to `.text.<symbol>`. Several
  items may name one section: the object then holds several sections of that
  name, which a linker merges. `#[section]` for a Mach-O target is an error.
- Code in the contract cannot rely on what it implements: a `tarn_rt_fault`
  whose body overflows an integer calls itself.

## Statics

```
static NAME Type = initializer
static mut NAME Type = initializer
static mut NAME Type                 // all bytes zero (.bss)
```

A static is fixed-layout global data. `Type` is an integer, float, `bool`, raw
pointer, fixed array (not empty), or non-generic struct of those. Structs use
declaration order and natural alignment, which is the C layout, with no
size limit (a 1 MiB table is fine; ordinary values stay limited to 64 KiB).
The initializer is evaluated at compile time: literals, `const` names and
arithmetic, `[N]T{...}` and `Struct{...}` literals giving every element and
field, `ffi.null()`, `ffi.from_address(n)`, and the name of another static
(stored as its address, a relocation). Anything else is E3085.

**Using a static's name as an expression yields its address**: `*T`, or
`*mut T` for `static mut`. Creating the address is safe; reading or writing
through it is `ffi.read`/`ffi.write` (or the volatile forms) inside `unsafe`,
like any raw pointer. Nothing ever borrows a static, so the ownership checker
has nothing to track and cannot protect you: the author owns data races,
aliasing and alignment. An immutable `static` lives in read-only memory; writing
through its address faults.

Reach a field or element by address arithmetic:

```tarn
import "ffi"

struct Header {
    magic u32
    flags u8
}

static mut HEADER Header = Header{magic: 1, flags: 2}

fn flags() u8 {
    field: *u8 := ffi.from_address(ffi.address(HEADER) + 4)
    unsafe { return ffi.read(field) }
}

fn main() {
    print(flags())
}
```

Every static is emitted whether or not it is used (a loader request is read by
no code); a static with `#[section]` is also marked retained
(`SHF_GNU_RETAIN`) so a linker's `--gc-sections` keeps it.

## Entry point

The ELF entry comes from the linker script (`ENTRY(kmain)`), from
`--entry kmain`, or, when neither names one and the program has `fn main`,
the default `tarn_main`. `--entry` overrides the script. The entry symbol
must be an `#[export]`ed function (or `tarn_main`).

## Linking

`tarn build --target x86_64-none --linker-script kernel.ld [--entry sym]
[-o kernel.elf]` writes a static ELF (default `<entry>.elf`). Without
`--linker-script` the relocatable object is written (default `<entry>.o`). The
build runs `LINKER -nostdlib -static -T kernel.ld [-e sym] -o kernel.elf
kernel.o`. Tarn bundles no linker and downloads nothing; the first match
of these is used:

1. the program named by the `TARN_LD` environment variable;
2. `ld.lld` on `PATH` (also looked for in Homebrew's `lld` and `llvm`
   directories);
3. a cross GNU linker for the architecture: `x86_64-elf-ld`,
   `x86_64-linux-gnu-ld`; `aarch64-elf-ld`, `aarch64-none-elf-ld`,
   `aarch64-linux-gnu-ld`;
4. `ld`, only on a Linux host of the same architecture.

If none exists the build fails naming what to install, and nothing is
written. If the linker fails its stderr is shown and no output is left. Tarn
does not pass `--gc-sections`; use `KEEP(...)` or `/DISCARD/` in the script.
