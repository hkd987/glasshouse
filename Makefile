# Glasshouse build entry points. `make help` lists every target.
#
# Day to day:
#   make test lint            host unit + integration tests, formatting, clippy, layering
#   make dev-host             run glassd on this machine (insecure dev backend)
#   make vm-up vm-wait        boot the FreeBSD dev VM (first time: freebsd-fetch freebsd-sysroot)
#   make e2e-vm               cross-compile, deploy to the VM, run the end-to-end suite
#   make test-env             first-time setup: tools, FreeBSD image, VM, deploy (see docs/dev/test-environment.md)

.DEFAULT_GOAL := help
SHELL := /bin/bash
REPO_ROOT := $(CURDIR)
FREEBSD_TARGET := x86_64-unknown-freebsd
# Binaries shipped in the OS image (see os/freebsd/dist.manifest).
SHIPPED := glassd glassctl glass-mcp glass-demo-agent glass-probe
# Cargo profile for FreeBSD builds. `vm` rebuilds in seconds for the VM loop;
# shippable images use `release` (make image-release). See
# docs/dev/build-performance.md.
FREEBSD_PROFILE ?= vm
export FREEBSD_PROFILE
export DIST_PROFILE := $(FREEBSD_PROFILE)

.PHONY: help build check test lint fmt check-layers deny sbom timings freebsd dev-host e2e-vm \
	test-env image-release ci clean

help: ## List targets
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Build every crate for this host (debug)
	cargo build --workspace

check: ## Type-check everything, fastest feedback (no codegen)
	cargo check --workspace --all-targets

test: ## Unit and integration tests on this host
	cargo test --workspace

lint: check-layers ## Formatting, clippy (warnings are errors), layering rules, generated art
	cargo fmt --all --check
	tools/branding/art.py --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --target $(FREEBSD_TARGET) -- -D warnings

fmt: ## Format all Rust code
	cargo fmt --all

check-layers: ## Enforce crate layering and manifest conventions
	tools/check-layers.py

deny: ## Check dependency licenses and advisories (needs cargo-deny)
	cargo deny check licenses bans sources

sbom: ## Software bill of materials, admins and security reviews only (BR-4); never ship it
	tools/legal/notices.py $(addprefix --package ,$(SHIPPED)) --sbom build/sbom/glasshouse.cdx.json

timings: ## Profile a clean build; opens target/cargo-timings/cargo-timing.html
	cargo build --workspace --timings

freebsd: ## Cross-compile the shipped binaries for FreeBSD (FREEBSD_PROFILE, default vm)
	cargo build --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED))

dev-host: ## Run glassd here with the insecure host sandbox (development only)
	tools/dev/host-run.sh

e2e-vm: freebsd vm-deploy ## Deploy to the FreeBSD VM and run the end-to-end suite
	tests/e2e/run-vm.sh

test-env: ## First-time setup of a full test environment (tools, image, VM, deploy)
	os/freebsd/scripts/check-host-deps.sh $(if $(filter 0,$(shell id -u)),--install)
	$(MAKE) freebsd-fetch freebsd-sysroot vm-up vm-wait freebsd vm-deploy
	@echo "Test environment ready. Try: make vm-ssh, then: su demo; glassctl agents"

image-release: ## Build a shippable image: release profile (thin LTO), baked qcow2
	$(MAKE) freebsd image FREEBSD_PROFILE=release

ci: lint test ## What CI runs on every change

clean: ## Remove build outputs (keeps downloaded FreeBSD images in build/cache)
	cargo clean
	rm -rf build/dist build/dev build/images

include os/freebsd/os.mk
