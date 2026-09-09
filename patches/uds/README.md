# uds (patched for OHOS)

Vendored from crates.io `uds 0.4.2` with one change: OHOS is excluded from the
"Linux uses `usize` for control-message lengths" condition, because its libc
defines those fields as `socklen_t` like musl does. See
`[patch.crates-io] uds` in the workspace manifest.
