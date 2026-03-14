# PHPantom — Configuration

Per-project configuration file for user preferences and optional features like diagnostic proxying.

## File

- **Name:** `.phpantom.toml`
- **Location:** Project root (next to `composer.json`).
- **Format:** TOML. Human-readable, supports comments, native Rust support via the `toml` crate.
- **Version control:** Up to each developer. The dot-prefix signals personal tooling config. Developers can gitignore it globally or per-project. PHPantom should never assume it is committed.

## Schema

```toml
# .phpantom.toml

[php]
# Override the detected PHP version.
# When unset, PHPantom infers from composer.json's platform or require.php.
# version = "8.3"

[composer]
# These record the user's answer to one-time prompts so PHPantom
# does not ask again on every session.

# Generate a minimal composer.json when the project has none.
# generate = true

[stubs]
# Install phpstorm-stubs into the project for projects without Composer.
# install = true

# Override which PHP extension stubs are loaded.
# When unset, PHPantom loads core + all commonly bundled extensions.
# extensions = ["Core", "standard", "json", "mbstring", "curl", "redis"]

[formatting]
# External formatter to proxy. Auto-detected when unset.
# tool = "php-cs-fixer"   # or "phpcbf" or "none"
# timeout = 10000

[diagnostics]
# Enable or disable native diagnostic providers.
# unresolved-member-access = true

[phpstan]
# PHPStan proxy. Runs PHPStan in editor mode on each file save.
# Auto-detected via vendor/bin/phpstan then $PATH. Set to "" to disable.
# command = "vendor/bin/phpstan"
# memory-limit = "1G"
# timeout = 60000
```

## Sections

### `[php]`

| Key       | Type   | Default       | Description                                |
|-----------|--------|---------------|--------------------------------------------|
| `version` | string | auto-detected | PHP version override (e.g. `"8.3"`, `"8.2"`) |

When unset, PHPantom reads the PHP version from `composer.json` (`config.platform.php` or `require.php`). This override exists for projects where `composer.json` is missing or inaccurate.

### `[composer]`

These fields are written by PHPantom when the user responds to a prompt. They can also be set by hand.

| Key        | Type | Default | Description                                             |
|------------|------|---------|---------------------------------------------------------|
| `generate` | bool | unset   | Whether to generate a minimal `composer.json` if missing |

When a key is unset, PHPantom will prompt the user. Once the user answers, PHPantom writes the value so the prompt does not appear again.

### `[stubs]`

| Key          | Type         | Default     | Description                                       |
|--------------|--------------|-------------|---------------------------------------------------|
| `install`    | bool         | unset       | Whether to install phpstorm-stubs for non-Composer projects |
| `extensions` | string array | auto-detect | Which PHP extension stubs to load (see below)     |

Same prompt-and-remember behaviour as the `[composer]` keys for `install`.

#### Extension stub selection

By default PHPantom loads stubs for PHP core and all bundled extensions
(matching the set that ships enabled in a stock PHP build), plus any
extensions declared in the project's `composer.json`. The `extensions`
key lets the user override this entirely.

##### Auto-detection from `composer.json`

When `extensions` is unset, PHPantom reads the `require` and
`require-dev` sections of the project's `composer.json` and collects
every `ext-*` key. These are added on top of the default set.

For example, if `composer.json` contains:

```json
{
    "require": {
        "php": "^8.2",
        "ext-redis": "*",
        "ext-imagick": "*"
    }
}
```

PHPantom loads the default bundled extensions plus `redis` and
`imagick` stubs automatically. No `.phpantom.toml` configuration
needed.

Only `composer.json` is read, not `composer.lock`. Transitive
`ext-*` requirements pulled in by dependencies are intentionally
ignored. Those extensions are used by vendor code, which PHPantom
already skips for diagnostics and does not complete into. If the
user's own code references an extension without declaring it in
`composer.json`, the correct fix is to add the `ext-*` requirement
(or override via `[stubs] extensions` in `.phpantom.toml`).

##### Manual override

```toml
[stubs]
extensions = [
  "Core", "standard", "json", "mbstring", "curl",
  "redis", "imagick", "mongodb",
]
```

When `extensions` is set, only the listed extensions are loaded.
The auto-detection from `composer.json` is skipped entirely. This
is useful when the user wants full control or when the project
has no `composer.json`.

The available extension names match the directory names in
phpstorm-stubs (e.g. `"redis"`, `"imagick"`, `"swoole"`, `"mongodb"`).
An unrecognised name is silently ignored with a log message.

**Implementation note:** The build script already embeds all stub files.
Filtering happens at runtime: when building the stub class/function
indices, skip entries whose source file path does not start with one
of the enabled extension directories. This is a simple string prefix
check on the relative path from `STUB_CLASS_MAP`.

### `[formatting]`

Controls formatting proxy behaviour. PHPantom does not ship a formatter;
it proxies requests to an external tool.

| Key       | Type   | Default     | Description                                        |
|-----------|--------|-------------|----------------------------------------------------|
| `tool`    | string | auto-detect | `"php-cs-fixer"`, `"phpcbf"`, or `"none"`          |
| `timeout` | int    | 10000       | Maximum runtime in milliseconds                    |

"Auto-detect" means PHPantom checks for `vendor/bin/php-cs-fixer` first,
then `vendor/bin/phpcbf`, then the tools on `$PATH`. The first one found
is used. Setting `tool = "none"` disables formatting entirely (PHPantom
does not register the capability).

### `[diagnostics]`

Controls native diagnostic providers.

| Key                        | Type | Default | Description                                      |
|----------------------------|------|---------|--------------------------------------------------|
| `unresolved-member-access` | bool | `false` | Report member access on unresolvable subject types |

### `[phpstan]`

Controls the PHPStan diagnostic proxy. PHPantom runs PHPStan in editor
mode (`--tmp-file` / `--instead-of`) on each file save and surfaces its
errors as LSP diagnostics. PHPStan runs in a dedicated background worker
that never blocks native diagnostics, with at most one process at a time.

| Key            | Type   | Default     | Description                                     |
|----------------|--------|-------------|-------------------------------------------------|
| `command`      | string | auto-detect | Path to `phpstan` binary. `""` disables.        |
| `memory-limit` | string | `"1G"`      | Memory limit passed to `--memory-limit`         |
| `timeout`      | int    | 60000       | Maximum runtime in milliseconds before killing  |

"Auto-detect" means PHPantom checks for `vendor/bin/phpstan` (respecting
Composer's `config.bin-dir`), then `phpstan` on `$PATH`. Setting
`command = ""` disables PHPStan entirely.

**Future external tools.** PHPMD, `php -l`, and Mago proxies are planned
but not yet implemented. Each will get its own `[tool]` section
following the same pattern as `[phpstan]`.

## Design decisions

1. **No global config.** Everything is per-project. Different projects have different tools, different PHP versions, different Composer setups. A global config would create confusing precedence rules.

2. **Prompt-and-remember pattern.** For one-time setup actions (generating `composer.json`, optimizing autoload, installing stubs), PHPantom asks once and records the answer. The user can change their mind by editing the file.

3. **Dedicated section per external tool.** Each proxied tool gets its own TOML section (e.g. `[phpstan]`) with tool-specific settings (`command`, `timeout`, `memory-limit`). A simple bool toggle was the original plan but proved insufficient once real-world configuration needs emerged.

4. **No editor or completion knobs.** PHPantom has no user-facing settings for completion behaviour today. Add sections when there is a real need, not speculatively.

## Implementation order

1. **Config writing.** When PHPantom prompts the user and gets an answer, write or update the relevant key. Preserve comments and formatting (use `toml_edit` crate).
2. **Diagnostic proxying.** Wire external tool proxies as each provider is implemented. *PHPStan proxy is done (`[phpstan]` section with `command`, `memory-limit`, `timeout`). PHPMD, `php -l`, and Mago proxies await implementation.*