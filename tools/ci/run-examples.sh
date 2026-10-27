#!/bin/sh
# Run every example with `tarn run` and diff its stdout against
# examples/expected/<name>.out. Examples that cannot run unattended are listed
# with a reason in examples/expected/skip.txt; an example in neither place fails.
# Usage: tools/ci/run-examples.sh [path/to/tarn]
set -u
tarn=$(realpath "${1:-target/release/tarn}")
root=$(cd "$(dirname "$0")/../.." && pwd)
expected="$root/examples/expected"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
for example in "$root"/examples/*.tarn "$root"/examples/*/main.tarn; do
    case "$example" in */main.tarn) name=$(basename "$(dirname "$example")") ;; *) name=$(basename "$example" .tarn) ;; esac
    [ "$name" = expected ] && continue
    if grep -q "^$name " "$expected/skip.txt"; then
        echo "skip $name: $(grep "^$name " "$expected/skip.txt" | cut -d ' ' -f 2-)"
        continue
    fi
    if [ ! -f "$expected/$name.out" ]; then
        echo "FAIL $name: add examples/expected/$name.out or list it in examples/expected/skip.txt"
        failed=1
        continue
    fi
    # Run from an empty directory with no stdin so examples cannot depend on either.
    if ! (cd "$work" && timeout 60 "$tarn" run "$example" < /dev/null > "$work/$name.out"); then
        echo "FAIL $name: tarn run exited with an error"
        failed=1
    elif ! diff -u "$expected/$name.out" "$work/$name.out"; then
        echo "FAIL $name: stdout differs from examples/expected/$name.out"
        failed=1
    else
        echo "ok   $name"
    fi
done
for name in $(grep -v '^#' "$expected/skip.txt" | cut -d ' ' -f 1); do
    [ -f "$root/examples/$name.tarn" ] || [ -f "$root/examples/$name/main.tarn" ] || { echo "FAIL skip.txt lists missing example $name"; failed=1; }
done
exit $failed
