# Baseline Model (Rust)

A desktop GUI application for analyzing data from a MATCH DSSD/BGO particle
detector. It parses the instrument's raw hex/binary data files, builds
histograms, fits peaks (Gaussian, Lorentzian, HEMG), and displays the
results as interactive plots across four modes: **Baseline**,
**Calibration**, **Flux**, and **Observation**.

Built with
[egui/eframe](https://github.com/emilk/egui) and organized as a two-crate
workspace:

- `baseline-core` — pure data-processing logic (parsing, fitting, math)
- `baseline-app` — the egui-based GUI

## Requirements

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain, edition 2021)

## Running the project

From the repository root:

```
cargo run -p baseline-app --release
```

This builds the `baseline-app` crate (and its `baseline-core` dependency)
and launches the GUI.

For a debug build:

```
cargo run -p baseline-app
```
