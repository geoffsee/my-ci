CONFIG ?= my-ci/workflows.toml

.PHONY: example build run list clean

example: run

build:
	cargo build

run: build
	cargo run -- --config $(CONFIG) run

list: build
	cargo run -- --config $(CONFIG) list

clean:
	cargo clean
