# RP-15 Linux
#
#   make          build the release binaries (needs cargo, libgtk-4-dev, libadwaita-1-dev)
#   make dist     build and package dist/rp15-linux-<version>-x86_64.tar.gz
#   make clean    remove build output
#
# The release tarball contains compiled binaries; installing it needs no Rust.

VERSION := $(shell cat VERSION)
ARCH := $(shell uname -m)
NAME := rp15-linux-$(VERSION)-$(ARCH)
DIST := dist/$(NAME)

.PHONY: all dist clean

all:
	cargo build --release --locked

dist: all
	rm -rf $(DIST) dist/$(NAME).tar.gz
	install -Dm755 target/release/rp15d $(DIST)/bin/rp15d
	install -Dm755 target/release/rp15-control $(DIST)/bin/rp15-control
	install -Dm644 -t $(DIST)/driver driver/Makefile driver/dkms.conf driver/*.c driver/*.h
	install -Dm644 -t $(DIST)/data data/*
	install -Dm644 -t $(DIST)/shell-extension shell-extension/*
	install -Dm755 install.sh $(DIST)/install.sh
	install -Dm644 README.md VERSION $(DIST)/
	tar -C dist -czf dist/$(NAME).tar.gz $(NAME)
	@echo "dist/$(NAME).tar.gz"

clean:
	cargo clean
	rm -rf dist
