# os/freebsd/os.mk: FreeBSD VM dev-loop targets; `include` it from the top-level Makefile.
# Loop: freebsd-fetch -> freebsd-sysroot -> vm-up -> vm-wait -> (cargo build) -> vm-deploy -> vm-run
# VM knobs (VM_CPUS, VM_MEM, VM_SSH_PORT, ...) are read from the environment; see scripts/lib.sh.

REPO_ROOT ?= $(CURDIR)
FREEBSD_SCRIPTS := $(REPO_ROOT)/os/freebsd/scripts

.PHONY: freebsd-host-deps freebsd-fetch freebsd-sysroot vm-up vm-down vm-status vm-wait \
	vm-ssh vm-run vm-reset vm-console vm-boot-timeline vm-deploy image

freebsd-host-deps: ## Check host tools for the FreeBSD VM loop
	$(FREEBSD_SCRIPTS)/check-host-deps.sh

freebsd-fetch: ## Download + verify the pinned FreeBSD VM image and base.txz
	$(FREEBSD_SCRIPTS)/fetch.sh

freebsd-sysroot: ## Build the cross-compile sysroot (build/freebsd/sysroot)
	$(FREEBSD_SCRIPTS)/sysroot.sh

vm-up: ## Boot the dev VM (QEMU, daemonized)
	$(FREEBSD_SCRIPTS)/vm.sh up

vm-down: ## Shut the dev VM down cleanly
	$(FREEBSD_SCRIPTS)/vm.sh down

vm-status: ## Show dev VM state
	$(FREEBSD_SCRIPTS)/vm.sh status

vm-wait: ## Block until the dev VM answers SSH and finished first boot
	$(FREEBSD_SCRIPTS)/vm.sh wait

vm-ssh: ## Interactive root shell in the dev VM
	$(FREEBSD_SCRIPTS)/vm.sh ssh

vm-run: ## Run CMD="..." as root in the dev VM
	@test -n "$$CMD" || { echo 'usage: make vm-run CMD="..."' >&2; exit 2; }
	$(FREEBSD_SCRIPTS)/vm.sh run "$$CMD"

vm-reset: ## Discard the dev VM disk (next vm-up is a fresh first boot)
	$(FREEBSD_SCRIPTS)/vm.sh reset

vm-console: ## Print the dev VM serial console log
	$(FREEBSD_SCRIPTS)/vm.sh console-log

vm-boot-timeline: ## Power-cycle the dev VM and print where boot time goes (RUNS=3, TRACE=1 per rc script)
	$(REPO_ROOT)/tools/boot/timeline.py --runs $(or $(RUNS),3) $(if $(TRACE),--trace)

vm-deploy: ## Stage build/dist/freebsd from built binaries and install it in the VM
	$(FREEBSD_SCRIPTS)/stage-dist.sh
	$(FREEBSD_SCRIPTS)/deploy.sh

image: ## Stage and bake build/images/glasshouse-<ver>-amd64.qcow2
	$(FREEBSD_SCRIPTS)/stage-dist.sh
	$(FREEBSD_SCRIPTS)/bake-image.sh
