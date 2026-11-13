# xdebbie — build, test, package, deploy.
#
#   make                 debug build of the workspace
#   make test            cargo test --workspace
#   make lint            fmt --check + clippy
#   make release         optimised xdebbied + xdctl
#   make deb             target/debian/xdebbied_<ver>-1_<arch>.deb (needs cargo-deb)
#   make install         install that .deb on this box
#   make deploy HOSTS="timesink gram"   scp + install the .deb on remote boxes
#   make tag             git tag v<ver> from Cargo.toml (annotated)
#   make push            git push --follow-tags
#
# Version comes from the workspace Cargo.toml. Bump it there first.

SHELL    := /bin/bash
CARGO    ?= cargo
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH     := $(shell dpkg --print-architecture 2>/dev/null || echo amd64)
DEB      := target/debian/xdebbied_$(VERSION)-1_$(ARCH).deb
HOSTS    ?=
SSH_USER ?= $(USER)

.DEFAULT_GOAL := build
.PHONY: build release test lint fmt clippy deb install deploy tag push clean version help

build:
	$(CARGO) build --workspace

release:
	$(CARGO) build --release -p xdebbied -p xdctl

test:
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

clippy: lint

# cargo-deb builds the release binaries itself (profile release, -p xdebbied).
deb:
	@$(CARGO) deb --version >/dev/null 2>&1 || { echo "cargo-deb missing: cargo install cargo-deb"; exit 1; }
	$(CARGO) deb -p xdebbied
	@ls -l $(DEB)

$(DEB): deb

install: $(DEB)
	sudo apt install -y ./$(DEB)

# make deploy HOSTS="timesink gram"  (ssh as $(SSH_USER); sudo must work there)
deploy: $(DEB)
	@test -n "$(HOSTS)" || { echo "usage: make deploy HOSTS=\"box1 box2\""; exit 1; }
	@for h in $(HOSTS); do \
	  echo "== $$h"; \
	  scp $(DEB) $(SSH_USER)@$$h:/tmp/ && \
	  ssh -t $(SSH_USER)@$$h "sudo apt install -y /tmp/$(notdir $(DEB)) && systemctl is-active xdebbied; xdebbied --version" || exit 1; \
	done

tag:
	@git diff --quiet || { echo "working tree not clean"; exit 1; }
	git tag -a v$(VERSION) -m "xdebbie v$(VERSION)"
	@echo "tagged v$(VERSION); now: make push"

push:
	git push --follow-tags

clean:
	$(CARGO) clean

version:
	@echo $(VERSION)

help:
	@sed -n '2,13p' Makefile | sed 's/^# \{0,1\}//'
