# envguard

A small CLI that does two jobs teams keep doing by hand:

1. **`envguard check`** compares your `.env` with `.env.example` and fails when a variable is missing, duplicated, or malformed.
2. **`envguard scan`** walks a project and flags hard-coded secrets: cloud keys, tokens, private keys, passwords in URLs and `password = "..."` assignments.

Written in Rust. No dependencies, one binary, exit codes made for CI.

## Install

```sh
cargo install --git https://github.com/vantacorehq/envguard
```

or build from source:

```sh
git clone https://github.com/vantacorehq/envguard
cd envguard
cargo build --release
# binary: target/release/envguard
```

## Usage

### Check an env file

```sh
envguard check                                  # .env vs .env.example
envguard check --env .env.production --example .env.example
envguard check --strict                         # warnings fail too
```

```
.env: error[missing-key]: STRIPE_KEY is defined in .env.example but missing in .env
.env:7: warning[extra-key]: LEGACY_FLAG is not documented in .env.example
.env:3: warning[empty-value]: SENTRY_DSN has an empty value

1 error, 2 warnings
```

| Rule | Severity | Meaning |
| --- | --- | --- |
| `missing-key` | error | key is in the example file but not in your env file |
| `duplicate-key` | error | the same key is defined twice in one file |
| `parse-error` | error | a line is not valid `KEY=VALUE` (bad name, unterminated quote, ...) |
| `extra-key` | warning | key is in your env file but not documented in the example |
| `empty-value` | warning | key is defined with an empty value |

The parser understands `export KEY=...`, single and double quotes, inline `# comments`, CRLF line endings and a UTF-8 BOM. Multi-line values are not supported.

### Scan for secrets

```sh
envguard scan                 # current directory
envguard scan path/to/repo
envguard scan --exclude fixtures --exclude docs
```

```
src/config.py:12: error[aws-access-key-id]: possible AWS access key ID (AKIA****)
deploy/.env.production:4: error[generic-secret]: possible hard-coded secret in 'DB_PASSWORD' (value ****)

2 errors, 0 warnings
```

Secrets are never printed in full: at most four characters are shown, and only for long values.

| Rule | Detects |
| --- | --- |
| `aws-access-key-id` | `AKIA...` / `ASIA...` access key IDs (the documented `...EXAMPLE` key is ignored) |
| `github-token` | `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` and `github_pat_` tokens |
| `slack-token` | `xoxb-`, `xoxp-`, `xoxa-`, `xoxr-`, `xoxs-` tokens |
| `stripe-secret-key` | `sk_live_` and `rk_live_` keys |
| `google-api-key` | `AIza...` keys |
| `private-key` | PEM private key headers |
| `credentials-in-url` | `scheme://user:password@host` (localhost and placeholders are ignored) |
| `generic-secret` | `password`, `secret`, `token`, `api_key`, ... assigned a real-looking value |

What it skips: `.git`, `target`, `node_modules`, `vendor`, `venv`, `dist`, `build`, binary files, files over 1 MiB, lock files, symlinks. In template files (`.env.example`, `*.sample`, `*.template`, ...) only the vendor-specific rules run, because placeholders are expected there.

A real `.env` in your working tree is scanned like any other file and will be flagged if it holds secrets. That is what you want in CI (a committed `.env` is the bug), but locally pass `--exclude .env`.

To silence a line, add `envguard:ignore` to it:

```python
API_KEY = "not-a-real-key-just-a-test-fixture"  # envguard:ignore
```

### Output formats

`--format text` (default), `--format json`, or `--format github` for inline annotations in GitHub Actions pull requests.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | no problems (warnings are fine unless `--strict`) |
| 1 | problems found |
| 2 | usage error or unreadable file |

## Use it in CI

GitHub Actions:

```yaml
- name: Check env files
  run: envguard check
- name: Scan for secrets
  run: envguard scan --format github
```

As a git pre-commit hook (`.git/hooks/pre-commit`):

```sh
#!/bin/sh
envguard scan --exclude .env || exit 1
envguard check --strict || exit 1
```

## Limits

envguard is a fast, dependency-free first line of defence, not a replacement for dedicated tools. It scans the files in your working tree, not git history, and it uses heuristics, so expect the occasional false positive (use `envguard:ignore`) and some misses (unquoted values in source code are not flagged). If a secret has been committed, rotate it, then remove it from history.

## Development

```sh
cargo test
cargo fmt
cargo clippy --all-targets
```

Everything lives in `src/main.rs`, split into small modules: `envfile` (parser), `compare` (check logic), `secrets` (scanner), `report` (output formats), `cli` (argument handling). Tests sit next to the code and run with `cargo test`.

## License

MIT
