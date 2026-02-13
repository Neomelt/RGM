# RGM: Rust GPU Monitor

[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)](https://github.com/xlqmu/RGM)
[![License: MIT / Apache-2.0](https://img.shields.io/badge/License-MIT%20%2F%20Apache--2.0-blue)](https://opensource.org/licenses/MIT)

A lightweight GPU monitoring utility built with Rust and egui. Supports **NVIDIA** (via NVML) and **AMD** (via sysfs/hwmon, including integrated GPUs). Simple, fast, and reliable.

## Features

*   **Multi-Vendor:** Automatically detects and monitors NVIDIA or AMD GPUs.
*   **iGPU Friendly:** Works with AMD integrated GPUs – unavailable sensors gracefully fall back to zero.
*   **Real-time Plots:** GPU utilization, memory, temperature, and power visualised over time.
*   **Low Overhead:** Built in Rust for maximum performance and minimal resource consumption.

## Prerequisites

Before you begin, ensure you have the following installed on your system:

1.  **Rust & Cargo:** If you don't have them, install them from [rust-lang.org](https://www.rust-lang.org/).
2.  **GPU Drivers (one of the following):**
    *   **NVIDIA:** Official NVIDIA drivers installed. Verify with `nvidia-smi`.
    *   **AMD:** The `amdgpu` kernel driver (included in most Linux kernels). Verify with `ls /sys/class/drm/card*/device/driver` pointing to `amdgpu`.

## Installation

1.  **Clone the repository:**
    ```bash
    git clone https://github.com/Xlqmu/RGM.git
    cd RGM
    ```

2.  **Build the optimized binary:**
    ```bash
    cargo build --release
    ```

The final executable will be located at `target/release/rgm`.

## Usage

Run the compiled application from your terminal to see the current GPU status.

```bash
./target/release/rgm
```

The application will auto-detect your GPU vendor and display real-time metrics.

---

## Troubleshooting

### NVIDIA

#### Error: `libnvidia-ml.so: cannot open shared object file`

This occurs when the application cannot find the NVIDIA Management Library (NVML), even if `nvidia-smi` works correctly.

**Solution:**

1.  **Find the NVML library path.**
    ```bash
    ldconfig -p | grep libnvidia-ml.so.1
    ```

2.  **Create a symbolic link.**
    ```bash
    sudo ln -s /lib/x86_64-linux-gnu/libnvidia-ml.so.1 /lib/x86_64-linux-gnu/libnvidia-ml.so
    ```

### AMD

#### Missing metrics (VRAM, fan speed, etc.)

Some AMD integrated GPUs do not expose all sysfs nodes. This is normal – RGM will display `0` for any unavailable metrics.

#### Permission denied reading sysfs

Running as a regular user should be sufficient for read-only monitoring. If you see permission errors, ensure your user has read access to `/sys/class/drm/card*/device/`.

## License

This project is licensed under either of:

*   Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
*   MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dually licensed as above, without any additional terms or conditions.