PREFIX ?= /usr/local
DESTDIR ?=
APP_ID := io.github.abdullahfanar.FlatBak
CARGO ?= cargo

.PHONY: all build release check test clippy fmt run install uninstall clean

all: build

build:
	$(CARGO) build

release:
	$(CARGO) build --release

check:
	$(CARGO) check --all-targets

test:
	$(CARGO) test

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

run:
	$(CARGO) run

install: release
	install -Dm755 target/release/flatbak $(DESTDIR)$(PREFIX)/bin/flatbak
	install -Dm644 data/$(APP_ID).desktop \
		$(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).svg \
		$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/$(APP_ID).metainfo.xml \
		$(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/$(APP_ID).xml \
		$(DESTDIR)$(PREFIX)/share/mime/packages/$(APP_ID).xml
	install -Dm644 LICENSE \
		$(DESTDIR)$(PREFIX)/share/licenses/$(APP_ID)/LICENSE
	@echo
	@echo "Installed. Refresh the caches with:"
	@echo "  update-desktop-database $(PREFIX)/share/applications"
	@echo "  update-mime-database $(PREFIX)/share/mime"
	@echo "  gtk4-update-icon-cache $(PREFIX)/share/icons/hicolor"

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/flatbak
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	rm -f $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(PREFIX)/share/mime/packages/$(APP_ID).xml
	rm -f $(DESTDIR)$(PREFIX)/share/licenses/$(APP_ID)/LICENSE

clean:
	$(CARGO) clean
