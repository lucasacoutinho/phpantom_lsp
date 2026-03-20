# PHPantom — Bug Fixes

Known bugs and incorrect behaviour. These are distinct from feature
requests — they represent cases where existing functionality produces
wrong results. Bugs should generally be fixed before new features at
the same impact tier.

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## Security

### ~~Tar archive path traversal in `build.rs`~~ ✅
- **Impact:** Medium · **Effort:** Low
- The build script downloads and extracts a tarball from GitHub. It strips
  the top-level directory component but does **not** validate that the
  resulting destination path remains inside the target directory. A
  maliciously crafted tarball (e.g. from a compromised upstream or MitM)
  could contain entries like `../../.cargo/config.toml` that escape the
  stubs directory.
- **Fixed:** Added path confinement check that rejects `..`, root, and
  prefix components in tar entries.  Also set
  `Archive::set_unpack_xattrs(false)` /
  `Archive::set_preserve_permissions(false)` for defence-in-depth.

### ~~Build-time HTTP download has no integrity verification~~ ✅
- **Impact:** Medium · **Effort:** Medium
- `build.rs` fetches `phpstorm-stubs` from the `master` branch with no
  checksum or signature verification. HTTPS provides transport security
  but nothing prevents a compromised upstream from serving malicious
  content. The `master` branch is a moving target.
- **Fixed:** Added `stubs.lock` pinning a specific commit SHA with a
  SHA-256 hash of the tarball.  `build.rs` downloads that exact commit
  and verifies the hash before extracting.  Run
  `scripts/update-stubs.sh` to update before a release.

### ~~Predictable temp file names (TOCTOU race)~~ ✅
- **Impact:** Low · **Effort:** Low
- `formatting.rs` and `phpstan.rs` construct temp file names using
  `process::id()` (and a timestamp), which are predictable. On
  multi-user systems a local attacker could pre-create a symlink at the
  predicted path to cause PHPantom to overwrite an arbitrary file.
- **Fixed:** Switched to `tempfile::NamedTempFile` which creates files
  with `O_EXCL`, preventing TOCTOU races.

### ~~Temp file cleanup not guaranteed on panics~~ ✅
- **Impact:** Low · **Effort:** Low
- In `formatting.rs` and `phpstan.rs`, if a panic occurs between temp
  file creation and the `remove_file` cleanup call the temp file is
  orphaned. The formatting handler runs inside `catch_panic_unwind_safe`
  but the temp file path is lost.
- **Fixed:** `tempfile::NamedTempFile` auto-deletes on drop (RAII),
  making cleanup panic-safe.