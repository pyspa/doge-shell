# Security audit — 2026-11-13

Baseline: merged `develop` commit `1a8cc357`. Scope: dependency advisories,
OAuth identity validation/storage, local HTTP serving, AI file tools, MCP
transport configuration, and command-authorization entry points. This is a
targeted source audit, not a claim that all vulnerabilities have been found.

## Confirmed issues and fixes

### AI file tools could follow an approval-time path replacement

Interactive `read_file`, `ls`, and `search` authorized a resolved path, then
used ordinary pathname-based I/O. A process able to modify the workspace could
replace an approved file or directory with a symlink while confirmation was
pending. Regression fixtures returned an outside sentinel before the fix.

Interactive reads and listings now use the existing descriptor-relative,
no-follow filesystem authority used by background tools. Search enumerates each directory through the same no-follow descriptor
authority, rechecks its root after confirmation, and uses bounded, no-follow
file reads for content. Unreadable, oversized or invalid-text files are reported
as an incomplete search rather than an exhaustive no-match result. The tests exercise final-file and parent-directory swaps; normal
in-workspace symlinks are still resolved before authorization.

### Local HTTP serving accepted arbitrary Host names

Binding `serve` to loopback did not prevent requests carrying an attacker-owned
Host name from receiving local files. This is the server-side prerequisite for
DNS rebinding; exploitation also depends on the victim's browser/network
protections and visiting an attacker-controlled site.

Router middleware now validates Host and URI authority against the configured
bind address or `localhost` for loopback. The guard also applies with `--cors`,
rejects duplicate Host headers, and covers absolute-form and HTTP/2 requests.
Hostless HTTP/1.0 remains supported. Custom DNS aliases are deliberately rejected.

### JWT optional time restrictions were not validated

The OAuth ID-token validator required expiry, issuer, audience and subject, but
left `validate_nbf` disabled. Signed tokens carrying a future `nbf` were accepted.
`nbf` is now validated when present, while omission remains supported. Tests
reject future and malformed values and retain valid signed-token coverage.
This does not imply that arbitrary unsigned or wrong-issuer tokens were accepted.

## Dependency findings

| Dependency | Finding / exposure | Remediation |
|---|---|---|
| `jsonwebtoken` 9.3.1 | [GHSA-h395-gr6q-cpjc](https://github.com/advisories/GHSA-h395-gr6q-cpjc): malformed optional claims could evade validation. Required expiry/issuer/audience checks already mitigated those particular claims here. | 10.4.0; minimum 10.3, existing AWS-LC crypto backend |
| bundled SQLite (`libsqlite3-sys` 0.38.2) | [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459.html): a malicious database/WAL pair can expose process memory when opened. | `rusqlite` 0.41.1 / `libsqlite3-sys` 0.39.1 |
| `rmcp` 3.5.1 | [RUSTSEC-2026-0462](https://rustsec.org/advisories/RUSTSEC-2026-0462.html): an untrusted HTTP MCP response could cause unbounded buffering. This client transport is enabled. | 3.9.0, minimum 3.9 |
| `rmcp` 3.5.1 | [RUSTSEC-2026-0457](https://rustsec.org/advisories/RUSTSEC-2026-0457.html) (OAuth SSRF) and [RUSTSEC-2026-0461](https://rustsec.org/advisories/RUSTSEC-2026-0461.html) (server schema-cache growth). The affected OAuth/server HTTP features are not enabled in this application. | Also covered by 3.9.0 |
| `rustix` 1.1.5 | [RUSTSEC-2026-0486](https://rustsec.org/advisories/RUSTSEC-2026-0486.html): unsound xattr buffer initialization; transitive dependency. | 1.1.6 |
| notification dependencies | Three unmaintained D-Bus packages; yanked `async-executor` and `clipboard-win` versions. | `notify-rust` 4.19.0 removes the old D-Bus/async packages; `clipboard-win` 5.4.3 |

The refreshed RustSec audit reports zero vulnerabilities and zero warnings.
The JWT advisory was separately checked against GitHub's advisory database,
because it was not present in this RustSec report. No advisories were ignored.

## Validation and limits

- Regression tests were observed failing before the source fixes and passing
  afterward for Host handling, approval-time file/directory replacement, and
  JWT time claims.
- `TMPDIR=/var/tmp RUST_TEST_THREADS=1 ./scripts/check.sh < /dev/null` passed
  after the dependency and primary source fixes (workspace tests, Clippy,
  formatting, guidance, architecture and portability checks).
- After the independent review's search follow-ups,
  `cargo test -p dsh-builtin --locked` passed: 1,310 tests, 2 ignored.
  The follow-ups cover outside-workspace ignore roots, nested whitelist rules,
  unreadable subtrees, and explicit incomplete-search reporting.
- The PR review found repeated ignore-file parsing per entry. Search now caches
  compiled rules for its lifetime and refreshes them on the next search. In the
  same release harness (2,000 files / 100 rules), ignore checks improved from
  2.50 seconds to about 3 milliseconds. Re-review found no further issues.
- Final `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `rustup run 1.95.0 cargo check --workspace --locked` passed. Portability,
  runtime authority, file budgets and diff checks also passed.
- Credential storage and command authorization were inspected but not redesigned.
- No real credentials or external target were used for exploit fixtures.
- macOS runtime behavior requires CI or a macOS host; Linux tests and portability
  checks alone do not establish macOS runtime coverage.
