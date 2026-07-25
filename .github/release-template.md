## Highlights
- Core improvements and user-visible value delivered in this release.
- Performance, stability, and packaging quality updates.

## What's Improved
- Monitoring accuracy and resilience enhancements.
- Linux desktop integration improvements for app discovery.
- Visual polish and branding updates.

## Installation
- Debian/Ubuntu: install the `.deb` package.
- Fedora/RHEL/openSUSE: install the `.rpm` package.
- Other Linux: extract `rgm-linux-x86_64.tar.gz` and run `rgm/rgm`.
- Verify downloads with `sha256sum -c --ignore-missing SHA256SUMS` (works when
  you downloaded only one of the artifacts).
- Binaries are built against glibc 2.34 and run on RHEL 9+, Ubuntu 22.04+,
  Debian 12+, and Fedora 36+.

## Upgrade Notes
- No breaking migration is expected for standard users.
- Reinstalling via package manager refreshes desktop launcher and icon cache.

## Known Issues
- If the app does not appear immediately in app launchers, log out/in once or run desktop cache refresh commands.

---
Auto-generated commit-level details are included below.
