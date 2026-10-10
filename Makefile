PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc
GENERATORDIR ?= $(SYSCONFDIR)/systemd/system-generators

.PHONY: all build install uninstall clean test

all: build

build:
	cargo build --release

install: build
	install -d $(DESTDIR)$(BINDIR)
	install -m 755 target/release/mvmctl $(DESTDIR)$(BINDIR)/mvmctl
	install -d $(DESTDIR)$(SYSCONFDIR)/mvmctl
	install -d $(DESTDIR)$(GENERATORDIR)
	ln -sf $(BINDIR)/mvmctl $(DESTDIR)$(GENERATORDIR)/mvmctl-generator
	@echo "Installed mvmctl to $(BINDIR)/mvmctl"
	@echo "Created Quadlet generator link at $(GENERATORDIR)/mvmctl-generator"
	@echo "Put VM JSON configs into $(SYSCONFDIR)/mvmctl/ and run 'systemctl daemon-reload'"

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/mvmctl
	rm -f $(DESTDIR)$(GENERATORDIR)/mvmctl-generator
	@echo "Uninstalled mvmctl"

clean:
	cargo clean
