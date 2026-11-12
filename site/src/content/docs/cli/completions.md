---
title: Completions and man page
description: Generate shell completion scripts for bash, zsh, fish, PowerShell, and elvish, and print the dbtl man page.
---

`dbtl completions <SHELL>` prints a completion script to stdout. `dbtl man` prints the man page in roff. Both read no data source, so they run anywhere.

## Shell completions

Supported shells are `bash`, `zsh`, `fish`, `powershell`, and `elvish`.

```sh
dbtl completions zsh    # completion script on stdout
```

Redirect the output to a file in a location your shell loads completions from.

```sh
dbtl completions bash > dbtl.bash
```

`completions` accepts no global flags. Passing `--manifest` or another data-source flag is a usage error.

## Man page

```sh
dbtl man > dbtl.1
```

`dbtl man` is hidden from `--help`. It exists to feed release packaging, not interactive discovery.

## Release archives

Each release archive ships both pre-generated: the completion scripts under `completions/` and the man page as `man/dbtl.1`. See [Installation](../getting-started/installation.md) for the archives.
