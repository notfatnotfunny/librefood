# Librefood — build + install (binary, .desktop, launcher icons)
#
#   make              # release build
#   make install      # install to $(PREFIX) (default: ~/.local)
#   make uninstall
#
# Override:  make install PREFIX=/usr/local
# Staging:   make install DESTDIR=/tmp/stage PREFIX=/usr

PREFIX  ?= $(HOME)/.local
DESTDIR ?=
BINDIR  := $(DESTDIR)$(PREFIX)/bin
APPDIR  := $(DESTDIR)$(PREFIX)/share/applications
ICONDIR := $(DESTDIR)$(PREFIX)/share/icons/hicolor

APPID   := org.librefood.Librefood
BINARY  := target/release/librefood
DESKTOP := data/$(APPID).desktop

.PHONY: all build install uninstall clean set-rotate

all: build

build:
	cargo build --release

# Persist preview rotate for all launches (drawer + CLI). Example: make set-rotate ANGLE=90
ANGLE ?= 90
set-rotate:
	mkdir -p $(HOME)/.config/librefood
	printf '%s\n' "$(ANGLE)" > $(HOME)/.config/librefood/camera-rotate
	@echo "Wrote $(HOME)/.config/librefood/camera-rotate → $(ANGLE) (env LIBREFOOD_CAMERA_ROTATE still wins)"

install: build
	install -Dm755 $(BINARY) $(BINDIR)/librefood
	install -Dm644 $(DESKTOP) $(APPDIR)/$(APPID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APPID).svg \
		$(ICONDIR)/scalable/apps/$(APPID).svg
	install -Dm644 data/icons/hicolor/256x256/apps/$(APPID).png \
		$(ICONDIR)/256x256/apps/$(APPID).png
	install -Dm644 data/icons/hicolor/128x128/apps/$(APPID).png \
		$(ICONDIR)/128x128/apps/$(APPID).png
	install -Dm644 data/icons/hicolor/64x64/apps/$(APPID).png \
		$(ICONDIR)/64x64/apps/$(APPID).png
	-update-desktop-database $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null
	-gtk-update-icon-cache -f -t $(DESTDIR)$(PREFIX)/share/icons/hicolor 2>/dev/null
	@echo "Installed librefood → $(PREFIX) (ensure $(PREFIX)/bin is on PATH)"

uninstall:
	rm -f $(BINDIR)/librefood
	rm -f $(APPDIR)/$(APPID).desktop
	rm -f $(ICONDIR)/scalable/apps/$(APPID).svg
	rm -f $(ICONDIR)/256x256/apps/$(APPID).png
	rm -f $(ICONDIR)/128x128/apps/$(APPID).png
	rm -f $(ICONDIR)/64x64/apps/$(APPID).png
	-update-desktop-database $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null
	-gtk-update-icon-cache -f -t $(DESTDIR)$(PREFIX)/share/icons/hicolor 2>/dev/null

clean:
	cargo clean
