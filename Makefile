# `make` builds the release binary and installs it to ~/.local/bin.
PREFIX ?= $(HOME)/.local

.PHONY: build check

build:
	cargo build --release
	install -Dm755 target/release/stonks $(PREFIX)/bin/stonks

check:
	cargo clippy --all-targets
	cargo test
