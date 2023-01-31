# doge-shell

Doge-shell is a high-performance shell written in Rust. Its fast and efficient design makes it a perfect choice for users who value speed and performance.

## Features

- Command completion like fish shell
- Scripting support for Lisp
- High-speed performance, thanks to being written in Rust
- Prompt with Git status display, similar to starship
- Comes bundled with a WASM runtime, allowing you to run WASM (WASI-compliant) binaries

## Installation

To install doge-shell, follow these steps:

```shell
$ git clone https://github.com/pyspa/doge-shell.git
$ cd doge-shell
$ cargo build --release
$ cargo install
```

Usage
To start using doge-shell, simply run:

```
$ dsh
```

License
Doge-shell is released under the MIT license. For more information, see LICENSE.
