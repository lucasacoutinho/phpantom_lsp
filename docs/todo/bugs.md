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

## B14. Redundant file re-parsing in unknown-member diagnostics

- **Impact:** Medium · **Effort:** Medium-High

The subject deduplication cache (per-pass `SubjectCache`) eliminated
the worst case where identical subjects were resolved hundreds of
times. However, each *unique* subject that goes through variable
resolution still calls `with_parsed_program`, which re-parses the
entire file from scratch. For unresolved subjects, the secondary
helpers (`resolve_scalar_subject_type`,
`resolve_unresolvable_class_subject`) add further re-parses. A
single unique untyped variable subject can trigger up to 6 full
re-parses of the file.

In files with many distinct variable subjects (e.g. different
`$var1->`, `$var2->`, `$var3->` accesses), the parsing cost still
adds up even with the subject cache.

### Fix — parse caching within a diagnostic pass

The file content is immutable during a single diagnostic pass.
Caching the parsed `Program` AST once and threading it through the
resolution calls would eliminate all redundant parsing, reducing
even the per-unique-subject cost. This is a larger refactor because
`with_parsed_program` is used across many modules and the `Program`
type borrows from a `bumpalo::Bump` arena that must stay alive.

