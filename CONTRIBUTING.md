# Contributing to RGM

Thanks for contributing.

## Development Standards
- Use clear, product-facing English in PR descriptions and release-facing docs.
- Prefer small, focused commits using Conventional Commit style (`feat:`, `fix:`, `chore:`, etc.).
- Keep behavior changes and refactors separated when possible.

## Local Validation (required)
Run before opening a PR:

```bash
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

If your change affects packaging, also validate package artifacts:

```bash
cargo build --release
cargo deb --no-build
cargo generate-rpm
```

## PR Requirements
- Explain the problem and the solution.
- Include user impact and rollback risk.
- Add screenshots for UI updates.
- Include a short Release Notes draft in English.

## Release Process
- Releases are tag-driven via `.github/workflows/release.yml`.
- Push a semantic tag like `v0.2.6` to trigger packaging and release publication.
- Keep `Cargo.toml` version aligned with release tag — the workflow fails fast
  on a mismatch (v0.2.3/v0.2.4 shipped packages labeled 0.2.2 before this gate).
- crates.io publishing is wired into release.yml but requires the
  `CARGO_REGISTRY_TOKEN` repository secret (Settings → Secrets → Actions;
  token from crates.io → Account Settings → API Tokens with publish scope).
  Until the secret exists the step skips with a warning and the crate stays
  at its last published version. After the first successful publish, remove
  the stale-version note from README Method 2.

## Commit Message Convention
Use Conventional Commits:
- `feat(scope): ...`
- `fix(scope): ...`
- `docs(scope): ...`
- `ci(scope): ...`
- `chore(scope): ...`

This keeps changelogs and release notes clean and reviewable.
