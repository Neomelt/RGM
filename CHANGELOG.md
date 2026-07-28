# Changelog

All notable changes to RGM are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the major version is 0, a minor bump may contain breaking changes.

## [Unreleased]

## [0.5.1] - 2026-07-28

### Fixed

- **`rgm --version` opened a window instead of printing a version.** Every
  argument was swallowed, so there was no way to ask an installed RGM which
  version it was — the first thing any update or bug report needs — and a typo
  like `rgm --verison` silently launched the GUI. `--version`/`-V` and
  `--help`/`-h` now print and exit; anything else is reported as a usage error
  with exit status 2 rather than ignored.

### Added

- An **Updating** section in the README, with a command that verifies the
  published checksum before installing.

## [0.5.0] - 2026-07-28

Answers "is my card throttling, and does it matter?" on NVIDIA.

### Added

- **Why clocks are held back, right now.** A line under the metrics reads the
  driver's clock-event reasons out loud, ranked by what you can act on:
  hardware slowdown and thermal limits are flagged, while sitting at the power
  limit is stated plainly — that is what a healthy card does under sustained
  load, not a fault. Reasons that describe no limit you are running into
  (idle clocks, application clock settings, sync boost, display clocks) are
  deliberately not reported.
- **How much it has cost you.** The same line carries the share of this
  session's *load* time that the power limit held clocks down, taken from the
  driver's own cumulative counter rather than integrated from samples, so
  throttling between samples is not missed. A session that never reached the
  limit says so, rather than reporting "0%" of something that did not happen.

The denominator is load time, not wall-clock time, and this is the part worth
knowing about: the driver's counter also advances while the GPU idles, and
while sampling is stalled by a driver error. Windows that were not observed
under load are discarded rather than counted, so the figure describes time you
were actually waiting on the GPU. It is withheld entirely until enough load
time has accumulated to mean anything.

### Notes

- NVIDIA only. AMD's throttler status lives in the versioned binary
  `gpu_metrics` blob, which this backend does not parse; both fields report
  "unavailable" rather than zero.
- Only the power policy is reported. The driver's thermal counter reads zero
  on consumer hardware and NVML documents it as unsupported, so no thermal
  share is claimed.

## [0.4.0] - 2026-07-28

Correctness release. No new metrics — the existing ones are measured and
labelled properly now, and the internals are prepared for what comes next.

### Fixed

- **The sampling loop ran at 6.99 Hz, not the documented 10 Hz.** Two
  `nvmlDeviceGetPcieThroughput` calls blocked for ~21 ms each (the driver
  averages over a fixed 20 ms window that cannot be configured), which made
  them 99% of every sample. They now refresh once per second and the reading
  is reused in between, cutting a sample from 45.4 ms to 6.05 ms on average.
- **Sampling now keeps a real 100 ms period.** The loop slept a fixed interval
  *after* doing its work, so the period was however long the work took plus
  100 ms. It sleeps to an absolute deadline instead: measured 99.96 ms mean
  (min 99.83, max 100.09) against 143 ms before. Falling behind resyncs rather
  than firing a catch-up burst.
- **Process names are no longer cut off.** `/proc/<pid>/comm` is truncated by
  the kernel at 15 bytes, which showed `xdg-desktop-por` for
  `xdg-desktop-portal-gnome`. Names are completed from `/proc/<pid>/cmdline`
  when — and only when — comm sits at the truncation limit and cmdline extends
  it, so interpreter processes keep their real name instead of becoming
  `python3`.
- **An empty process table no longer lies.** The AMD backend cannot enumerate
  per-process memory, but the table still drew its headers, which reads as
  "nothing is using the GPU". It now says which of the two is true.
- **Wayland windows have an `app_id`.** RGM never set one, so sway and Hyprland
  had nothing to match a window rule against and compositors could not link the
  window to `rgm.desktop`. `StartupWMClass` is set for the same reason on X11.

### Changed

- `rgm_ui` no longer exposes a library API. `src/lib.rs` had made `app`, `data`
  and `monitor` public, so every internal change was technically a breaking one
  for a library nobody imports. The binary is unaffected; `cargo install
  rgm_ui` works exactly as before.
- Dead fields removed with it: `GpuInfo::uuid`, `GpuInfo::vbios_version` (both
  collected on every start and never displayed) and `ProcessInfo::cpu_percent`
  (always 0.0).

### Upgrade notes

- On X11 the `WM_CLASS` instance is now empty (`"", "rgm"` instead of
  `"rgm", "rgm"`) — an egui-winit limitation, since it hardcodes an empty
  instance and the field is shared by both Linux backends. Rules matching on
  the class (`i3`'s `class=`, `StartupWMClass`) are unaffected; a rule
  matching on the instance is not, and there is no way to set the two
  independently through eframe.
- Saved window geometry is preserved. eframe lowercases the app id when it
  builds its state directory on Linux, so the old app name `RGM` and the new
  `app_id` `rgm` both resolve to `~/.local/share/rgm`.

## [0.3.0] - 2026-07-25

### Added

- Dashboard redesign: stat cards with fill bars, one sparkline per metric, and
  a `10s ago / 5s ago / now` time axis.
- Device count reporting, so a multi-GPU machine is labelled "GPU 0 of N".

### Fixed

- NVML samples degrade per metric instead of failing whole when one sensor is
  unavailable.
- Compute (CUDA) processes are included in the process table, not just
  graphics ones.
- An error screen replaces the panic when no supported GPU is found.
- Stale and never-arriving samples are both surfaced in the UI.
- Sparklines share a uniform y-axis width so stacked plots stay aligned.

### Changed

- Repaint is throttled to the sampling cadence.
- Unused `tokio`, `serde` and `serde_json` dependencies dropped; `Arc<Mutex>`
  wrappers removed.
- CI gained a build/test/clippy gate; releases pin a glibc floor, verify the
  tag against `Cargo.toml`, and publish checksums.

## [0.2.5] - 2026-02-25

### Added

- Launcher icon set and a desktop entry, with icon and desktop database
  refreshes on install and removal.

### Fixed

- The memory plot is guarded against a zero total VRAM reading.
- NVML static info hardened and PCIe metrics corrected.

## [0.2.0] - 2026-02-13

### Added

- First public release: real-time NVIDIA monitoring via NVML, AMD support via
  amdgpu sysfs, and the CI/CD release workflow.

[Unreleased]: https://github.com/Neomelt/RGM/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/Neomelt/RGM/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Neomelt/RGM/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Neomelt/RGM/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Neomelt/RGM/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/Neomelt/RGM/compare/v0.2.0...v0.2.5
[0.2.0]: https://github.com/Neomelt/RGM/releases/tag/v0.2.0
