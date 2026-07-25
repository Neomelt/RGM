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
- Verify downloads with `sha256sum -c SHA256SUMS`.
- Binaries are built on Ubuntu 22.04 and require glibc 2.35 or newer
  (Ubuntu 22.04+, Debian 12+, Fedora 36+; RHEL 9 ships glibc 2.34 and is
  not covered — build from source there).

## Upgrade Notes
- No breaking migration is expected for standard users.
- Reinstalling via package manager refreshes desktop launcher and icon cache.

## Known Issues
- If the app does not appear immediately in app launchers, log out/in once or run desktop cache refresh commands.

---
Auto-generated commit-level details are included below.
