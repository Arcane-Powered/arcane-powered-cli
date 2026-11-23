# Arcane Powered CLI (`arcane`)

Single static Rust binary for uploading and managing game builds on **Arcane Powered**.

- **Zero runtime dependencies** (`rustls-tls`, no OpenSSL, no Node.js required)
- **FastCDC v2.2 delta deduplication** — bit-for-bit identical Gear table & masks to `arcane-builds.mjs` (512 KiB–8 MiB chunks)
- **Multi-threaded SHA-256 & zstd** (level 3, 92% threshold) via `rayon`
- **Local mtime+size hash cache** — unchanged files on large hotfixes are never re-read from disk
- **CI-friendly** — automatic non-interactive output, `--json` mode, env var overrides, and official GitHub Action

## Installation

### macOS & Linux

```sh
curl -fsSL https://raw.githubusercontent.com/Arcane-Powered/arcane-powered-cli/main/install.sh | sh
```

### From source (Cargo)

```sh
cargo install --git https://github.com/Arcane-Powered/arcane-powered-cli
```

Prebuilt static binaries and `SHA256SUMS.txt` are published on [GitHub Releases](https://github.com/Arcane-Powered/arcane-powered-cli/releases) for Linux (`x86_64`/`aarch64` musl), macOS (`x86_64`/`arm64`), and Windows (`x86_64`).

## Quick Start

```sh
# 1. Authenticate (saved to ~/.config/arcane/config.json with mode 600)
arcane login

# 2. List your studio's games and add-ons
arcane titles

# 3. Initialize arcane.json in your project root
arcane init --title <TITLE_ID>

# 4. Push a build
arcane push ./build/windows --platform windows --version 1.0.0
```

In CI, set `ARCANE_API_KEY` and `ARCANE_PORTAL_URL` — environment variables always override `~/.config/arcane/config.json`.

## Commands

| Command | Description |
| --- | --- |
| `arcane login` | Save API key & portal URL (verifies access by default) |
| `arcane logout` | Remove saved credentials |
| `arcane whoami` | Show active portal URL, masked key, and credential source |
| `arcane init` | Generate `arcane.json` at project root |
| `arcane titles` | List studio games and add-ons |
| `arcane push [folder]` | Chunk, deduplicate, compress, upload, and publish a build |
| `arcane jobs [--title <id>]` | List recent publish jobs (running, done, error) |
| `arcane completions <shell>` | Generate shell completions (`bash`, `zsh`, `fish`, `powershell`) |

All commands accept `--json` for machine-readable output.

## `arcane.json`

```json
{
  "title": "00000000-0000-0000-0000-000000000000",
  "chunking": "cdc",
  "compress": true,
  "concurrency": 6,
  "ignore": ["*.pdb", "*.dSYM", ".git", ".DS_Store"],
  "platforms": {
    "windows": "./build/windows",
    "macos": "./build/macos",
    "linux": "./build/linux"
  }
}
```

When `arcane.json` is present, `folder` and `--title` can be omitted from `arcane push`. If `--version` is omitted, `arcane` reads the exact git tag (`git describe --tags --exact-match`) or `GITHUB_REF_NAME`.

## GitHub Action

```yaml
- uses: Arcane-Powered/arcane-powered-cli@v1
  with:
    api-key: ${{ secrets.ARCANE_API_KEY }}
    portal-url: ${{ vars.ARCANE_PORTAL_URL }}
    title: ${{ vars.ARCANE_TITLE_ID }}
    platform: windows
    version: ${{ github.ref_name }}
    folder: ./build/windows
    draft: true
    create-notes: true
```

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```
