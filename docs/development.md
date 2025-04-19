# Development Guide

## Environment Setup

The rk8s project is organized and managed as a monorepo, using Buck2 for building. We provide a ready-to-use standard development environment (download link: https://file.gitmega.net/r2cn/r2cn.zip). This environment is based on Debian 12 and comes pre-installed with all the dependencies required to build the project. The default username, password, and root password are all set to `r2cn`.

If you prefer to set up your own development environment, follow these steps:

- Install Rust:

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

  After installation, ensure that the `~/.cargo/bin` directory is added to your `PATH` environment variable.

- Install Buck2:

  ```bash
  export ARCH=$(uname -m) 
  curl https://github.com/facebook/buck2/releases/download/latest/buck2-${ARCH}-unknown-linux-gnu.zst --output /tmp/buck2-${ARCH}-unknown-linux-gnu.zst --location
  zstd -d /tmp/buck2-${ARCH}-unknown-linux-gnu.zst -o $HOME/.cargo/bin/buck2
  chmod +x $HOME/.cargo/bin/buck2
  ```

- Install Reindeer:

  ```bash
  cargo install --locked --git https://github.com/facebookincubator/reindeer reindeer
  ```

- Install System Tools and Dependencies (on Debian-based Linux distributions):

  ```bash
  sudo apt-get install build-essential clang lld pkg-config
  sudo apt-get install seccomp libseccomp-dev
  ```

## Building the Project

Clone the project repository:

```bash
git clone https://github.com/r2cn-dev/rk8s.git
cd rk8s
```

Fetch third-party dependencies:

```bash
reindeer --third-party-dir third-party vendor
reindeer --third-party-dir third-party buckify
```

Build all targets in the repository:

```bash
buck2 build //project/...
```

Alternatively, list all available build targets and build only the ones you need:

```bash
buck2 targets //...
```

## Dependency Management

For detailed information on managing third-party dependencies, refer to the `third-party/README.md` file.

## Code Style and Clippy Checks

The rk8s project uses `rustfmt` to enforce a consistent code style. Before submitting your changes, please format your code by running the following command:

```bash
cd project
cargo fmt --package <PACKAGE_NAME>
```

You can find the `<PACKAGE_NAME>` in the `[workspace]` section of the `project/Cargo.toml` file. Note that you only need to run the above command for the packages you have modified.

Additionally, you must run Clippy to check for potential issues in the code:

```
cargo clippy --workspace -- -D warnings
```

Please ensure that you run the above commands before submitting your code and resolve any warnings or errors that are reported.

## Contributing

For more information on how to contribute to the project, including creating pull requests, code reviews, and testing requirements, please refer to the [Contributing Guide](./contributing.md).