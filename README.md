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

## config.lisp

Users can extend the functionality with their own lisp scripts.
An example is shown below.

```lisp

;; Define alias

(alias "a" "cd ../")
(alias "aa" "cd ../../")
(alias "aaa" "cd ../../../")
(alias "aaaa" "cd ../../../../")
(alias "ll" "exa -al")
(alias "cat" "bat")
(alias "g" "git")
(alias "gp" "git push")
(alias "m" "make")

;; It has a direnv equivalent.
(allow-direnv "~/repos/github.com/pyspa/doge-shell")

;; User functions
(fn gco ()
    (vlet ((slct (sh "git branch --all | grep -v HEAD | sk | tr -d ' ' "))
           (branch (sh "echo $slct | sed 's/.* //' | sed 's#remotes/[^/]*/##'")))
          (sh "git checkout $branch")))

(fn fkill (arg)
    (vlet ((q arg)
           (slct (sh "ps -ef | sed 1d | sk -q $q | awk '{print $2}' ")))
        (sh "kill -TERM $slct")))

```

License
Doge-shell is released under the MIT license. For more information, see LICENSE.
