---
title: Installation
description: Install dbtl with Homebrew, a prebuilt release binary, or Cargo, and set up shell completions and the man page.
---

Install `dbtl` with Homebrew, a prebuilt binary, or from source. The result is a single `dbtl` binary.

## Homebrew

Homebrew works on macOS and Linux.

```sh
brew install hytfjwr/tap/dbtl
```

## Prebuilt binaries

Download the archive for your platform from the [releases page](https://github.com/hytfjwr/dbtl/releases).

| Platform | Artifact |
|---|---|
| macOS (Apple Silicon / Intel) | `*-aarch64-apple-darwin.tar.gz` / `*-x86_64-apple-darwin.tar.gz` |
| Linux (x86_64 / ARM64) | `*-x86_64-unknown-linux-gnu.tar.gz` / `*-aarch64-unknown-linux-gnu.tar.gz` |
| Linux, fully static (x86_64 / ARM64, musl) | `*-x86_64-unknown-linux-musl.tar.gz` / `*-aarch64-unknown-linux-musl.tar.gz` |
| Windows (x86_64) | `*-x86_64-pc-windows-msvc.zip` |

Each archive contains the `dbtl` binary, `LICENSE`, `README.md`, shell completions (`completions/`), and a man page (`man/dbtl.1`).

### Verify the download

Verify the download against `checksums.txt` and the build provenance before you unpack it:

```sh
sha256sum --check --ignore-missing checksums.txt
gh attestation verify dbtl-@DBTL_TAG@-aarch64-apple-darwin.tar.gz --repo hytfjwr/dbtl
```

## From source

Building from source needs Rust 1.88 or newer.

```sh
git clone https://github.com/hytfjwr/dbtl.git
cd dbtl
cargo install --path .
```

## Shell completions and man page

`dbtl completions <SHELL>` prints a completion script for `bash`, `zsh`, `fish`, `powershell`, or `elvish`. `dbtl man` prints the man page in roff.

The release archives ship both pre-generated, under `completions/` and `man/dbtl.1`. See [Completions](../cli/completions.md) for setup.

## Next steps

Open a project with the [quick start](quick-start.md).
