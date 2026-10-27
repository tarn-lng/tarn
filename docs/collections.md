# Collections and algorithms (Phase 31)

`import "collections"` loads ordinary bundled Tarn source; a local module may
override it. No collection has native ownership privileges. Design:
[ADR 0060](adr/0060-general-purpose-foundations.md).

## Keys and hashing

`core.Hash` is nominal: `fn hash(&self) u64`. A Map/Set key needs `Hash + Eq`.
Equal keys must hash equally, and Eq must define an equivalence relation. Do not
change hash/equality-relevant state of a stored key. Integers, bool and strings
have implementations in core. User types implement both interfaces in their own
module. Floats have no Hash/Eq implementation for keys.

The default hash is not cryptographic or randomized. Adversarial collisions can
make operations linear in the number of entries. Hash values, bucket placement
and iteration order are unspecified across compiler versions and must not be
persisted or used as security checks.

## Map

| Operation | Behavior |
|---|---|
| `Map.new()` | Empty owned map; type arguments inferred from context |
| `len()`, `is_empty()` | Entry count/emptiness |
| `insert(key, value)` | Moves both inputs in; returns owned `Option<V>` of the replaced value; replaces the stored equal key too |
| `contains(&key)` | Tests presence without returning a loan |
| `get(&key)` | Direct `&V` loan; missing keys abort |
| `get_mut(&key)` | Direct `&mut V` loan; missing keys abort |
| `remove(&key)` | Moves the value out as `Option<V>`; destroys the removed key |
| `slot_count()`, `has_entry(slot)` | Inspect unspecified table positions; out-of-range has_entry is false |
| `entry_at(slot)` | Direct shared loan of Entry; empty/out-of-range positions abort |
| `drain()` | Moves entries to a Vec, empties the map and releases its old table |

References returned by get/get_mut/entry_at borrow the map. A temporary lookup
key does not need to survive the result. An outstanding loan prevents structural
mutation, movement or destruction of the map. There are no Option-wrapped
references or iterators storing borrowed state.

```tarn
import "collections"
fn main() {
    var counts: collections.Map<string, u64> = collections.Map.new()
    counts.insert("eth0", 1)
    if counts.contains(&"eth0") {
        value := counts.get_mut(&"eth0")
        *value = *value + 1
    }
    print(counts.get(&"eth0"))
}
```

Slots have power-of-two capacity and load at most 3/4. Growth moves entries;
removal repairs the following probe cluster without tombstones. Normal lookup
is expected O(1) with well-distributed hashes; no worst-case constant-time claim
is made. Capacity arithmetic/allocation retains existing abort semantics.

## Set

Set is an owned Map-backed key set. `insert(key)` returns true for a new key and
false for replacement. `contains(&key)` and `remove(&key)` return bool;
`len`/`is_empty` query size. `drain()` returns owned keys in unspecified order.

## Algorithms

`sort(&mut Vec<T>, fn(&T, &T) i32)` performs unstable in-place heapsort with
O(n log n) worst-case comparisons and O(1) auxiliary element storage. It works
with non-Copy owners. Comparator sign indicates less/equal/greater; comparison
must be consistent. No subtraction-based integer comparator is necessary.

`binary_search(&[]T, &T, fn(&T, &T) i32)` returns the first equal index as
Option<usize>, or None. Input must already be sorted with the same comparator.
It uses O(log n) comparisons. These APIs do not provide stable sorting,
deques/trees, a general iterator framework or a generic mutable-slice swap API.

## Reference places and modular arithmetic

`*reference` reads a Copy referent; reading a non-Copy referent is E3079.
`&*reference` borrows and `*mutable_reference = value` assigns a place.
Dereferencing nonreferences is E3080; raw pointers remain accessible only through
unsafe FFI helpers. Pattern projection keeps ADR 0018's existing semantics.

Integer `wrapping_add`, `wrapping_sub` and `wrapping_mul` explicitly compute
modulo the integer width. Ordinary operators and numeric casts remain checked.

## Recoverable lookup and cursors

`collections.get_copy(&map, &key)` returns `Option<V>` for Copy values.
It is a free function because conditional method bounds are unsupported.
`map.get_or(&key, &fallback)` returns an ordinary loan of the map or fallback;
conservative result provenance includes both. Existing get/get_mut keep their
abort-on-missing contract. `next_slot(&mut cursor)` skips empty slots and returns
an occupied offset for entry_at; start at zero, keep the map unchanged during
traversal. It allocates nothing and stores no references.
