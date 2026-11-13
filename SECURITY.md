# Security policy

## Supported versions

The [latest release](https://github.com/hytfjwr/dbtl/releases/latest) is the only version that receives security fixes. Update to it before reporting a vulnerability that an upgrade might already fix.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/hytfjwr/dbtl/security/advisories/new) — do not open a public issue, PR, or discussion for them. Include the affected version, the data source or input that triggers the problem, and what an attacker could gain.

You can expect an acknowledgement within a few days. After the fix lands, a GitHub security advisory is published with the release that carries it.

For bugs that are not vulnerabilities, use the [issue tracker](https://github.com/hytfjwr/dbtl/issues/new/choose).

## Scope notes

`dbtl` parses `manifest.json`, project SQL/YAML, and theme files from wherever you point it — treat untrusted project checkouts as untrusted input, as with any dbt or compiler tool. Export keys write files into the current working directory only; node names from a manifest are sanitized before becoming file names. `dbtl` makes no network calls at runtime.
