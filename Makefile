# Glasshouse build entry points. `make help` lists every target.
#
# Day to day:
#   make test lint            host unit + integration tests, formatting, clippy, layering
#   make dev-host             run glassd on this machine (insecure dev backend)
#   make vm-up vm-wait        boot the FreeBSD dev VM (first time: freebsd-fetch freebsd-sysroot)
#   make e2e-vm               cross-compile, deploy to the VM, run the end-to-end suite

.DEFAULT_GOAL := help
SHELL := /bin/bash
REPO_ROOT := $(CURDIR)
FREEBSD_TARGET := x86_64-unknown-freebsd
# Binaries shipped in the OS image (see os/freebsd/dist.manifest).
SHIPPED := glassd glassctl glass-mcp glass-demo-agent glass-probe

.PHONY: help build test lint fmt check-layers deny freebsd dev-host e2e-vm ci clean

help: ## List targets
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Build every crate for this host (debug)
	cargo build --workspace

test: ## Unit and integration tests on this host
	cargo test --workspace

lint: check-layers ## Formatting, clippy (warnings are errors) and layering rules
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --target $(FREEBSD_TARGET) -- -D warnings

fmt: ## Format all Rust code
	cargo fmt --all

check-layers: ## Enforce crate layering and manifest conventions
	tools/check-layers.py

deny: ## Check dependency licenses and advisories (needs cargo-deny)
	cargo deny check licenses bans sources

freebsd: ## Cross-compile the shipped binaries for FreeBSD (release)
	cargo build --release --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED))

dev-host: ## Run glassd here with the insecure host sandbox (development only)
	tools/dev/host-run.sh

e2e-vm: freebsd vm-deploy ## Deploy to the FreeBSD VM and run the end-to-end suite
	tests/e2e/run-vm.sh

ci: lint test ## What CI runs on every change

clean: ## Remove build outputs (keeps downloaded FreeBSD images in build/cache)
	cargo clean
	rm -rf build/dist build/dev build/images

include os/freebsd/os.mk
