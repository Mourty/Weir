# Weir build and installation.
#
#   make              build the release binaries
#   make install      install for the current user (no root needed)
#   make uninstall    remove every file install put down
#   make test         run the unit tests
#   make fuzz         hand made-up input to one reader for ten minutes (fuzz/README.md)
#   make icons        redraw the window icon from packaging/weir.svg
#   make licenses     gather the libraries' license notices (cargo-about)
#   make rpm          build an RPM package (Fedora, Nobara)
#   make deb          build a .deb package (Ubuntu, Mint, Pop!_OS, Debian)
#
# The default prefix is ~/.local, which needs no root and whose bin directory
# is already on PATH on Fedora and Nobara. For a system-wide install:
#
#   sudo make install PREFIX=/usr/local
#
# Package builds pass DESTDIR to stage the files into a build root.

PREFIX ?= $(HOME)/.local
DESTDIR ?=

BINDIR := $(PREFIX)/bin
DATADIR := $(PREFIX)/share
APPDIR := $(DATADIR)/applications
ICONDIR := $(DATADIR)/icons/hicolor/scalable/apps
SYMBOLIC_ICONDIR := $(DATADIR)/icons/hicolor/symbolic/apps
METAINFODIR := $(DATADIR)/metainfo
METAINFO := io.github.mourty.weir.metainfo.xml
DOCDIR := $(DATADIR)/doc/weir
LICENSEDIR := $(DATADIR)/licenses/weir

# systemd reads user units from ~/.local/share/systemd/user and from
# /usr/lib/systemd/user, so a system prefix needs this overridden:
#   sudo make install PREFIX=/usr SYSTEMD_USER_DIR=/usr/lib/systemd/user
SYSTEMD_USER_DIR ?= $(DATADIR)/systemd/user

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
RPMTOP ?= $(HOME)/rpmbuild
# Extra flags for rpmbuild, e.g. RPMBUILD_FLAGS=--nodeps to skip the
# BuildRequires check when testing on a non-Fedora machine.
RPMBUILD_FLAGS ?=
# A timestamp release means every local build outranks the last one, so
# `dnf install` always upgrades rather than refusing as already installed.
# The := on the second line pins it: without that, $(shell date) would re-run
# on every reference and the install target would look for a different
# filename than the build target produced.
RPM_RELEASE ?= $(shell date +%Y%m%d%H%M)
RPM_RELEASE := $(RPM_RELEASE)

CARGO ?= cargo
CARGO_FLAGS ?= --release
BINARIES := weir-daemon weir weirctl

.PHONY: all build test fuzz install uninstall clean help rpm rpm-install deb appimage appimage-dir icons licenses

all: build

help:
	@echo "make            build the release binaries"
	@echo "make install    install for the current user into $(PREFIX)"
	@echo "make uninstall  remove every installed file"
	@echo "make test       run the unit tests"
	@echo "make fuzz TARGET=import  fuzz one reader (needs nightly and cargo-fuzz)"
	@echo "make clean      delete the build directory"
	@echo "make rpm        build an RPM package into $(RPMTOP)/RPMS"
	@echo "make rpm-install  build the RPM and install it with dnf"
	@echo "make deb        build a .deb package into target/deb"
	@echo "make appimage   build an AppImage into target/appimage (needs appimagetool)"
	@echo "make appimage-dir  lay out the AppImage's contents, without packing them"
	@echo "make icons      redraw the window icon (needs rsvg-convert)"
	@echo "make licenses   gather the libraries' license notices (needs cargo-about)"

# DESTDIR is left out of the build's environment: libopus builds with
# CMake, whose install step honors it, so `make install DESTDIR=...` run
# before a build (as `make deb` does) would put the library in the
# staging folder, where the build cannot find it.
build:
	env -u DESTDIR $(CARGO) build $(CARGO_FLAGS)

test:
	$(CARGO) test --workspace

clean:
	$(CARGO) clean

# The window shows a PNG of the icon, built into the program. Redraw it after
# changing packaging/weir.svg.
icons:
	rsvg-convert -w 256 packaging/weir.svg -o crates/gui/assets/weir.png

# The licenses of the libraries built into Weir ask that their notices go
# with every copy, so packages carry this file. Run it after changing
# dependencies; CI fails when it is out of date.
licenses:
	cargo about generate --workspace --locked --fail \
		-c packaging/licenses/about.toml packaging/licenses/about.hbs \
		-o packaging/licenses/THIRD-PARTY-LICENSES.txt

# Fuzzing, by hand: TARGET is one of fuzz/fuzz_targets, SECONDS how long.
# The noise suppressor keeps an FFT cache for good, which the leak check
# would report at the end of every mixer run, during it and at the exit.
TARGET ?= import
SECONDS ?= 600
NO_LEAKS = $(if $(filter mixer,$(TARGET)),yes)
fuzz:
	mkdir -p fuzz/corpus/$(TARGET)
	$(if $(NO_LEAKS),ASAN_OPTIONS=detect_leaks=0) \
	cargo +nightly fuzz run -O $(TARGET) fuzz/corpus/$(TARGET) fuzz/seeds/$(TARGET) -- \
		-max_total_time=$(SECONDS) -timeout=10 -rss_limit_mb=2048 -malloc_limit_mb=4200 \
		$(if $(NO_LEAKS),-detect_leaks=0)

install: build
	install -d $(DESTDIR)$(BINDIR)
	for b in $(BINARIES); do install -m755 target/release/$$b $(DESTDIR)$(BINDIR)/$$b; done
	install -d $(DESTDIR)$(APPDIR)
	sed 's|@BINDIR@|$(BINDIR)|g' packaging/weir.desktop.in \
		> $(DESTDIR)$(APPDIR)/weir.desktop
	chmod 644 $(DESTDIR)$(APPDIR)/weir.desktop
	install -Dm644 packaging/weir.svg $(DESTDIR)$(ICONDIR)/weir.svg
	install -Dm644 packaging/weir-symbolic.svg $(DESTDIR)$(SYMBOLIC_ICONDIR)/weir-symbolic.svg
	install -Dm644 packaging/$(METAINFO) $(DESTDIR)$(METAINFODIR)/$(METAINFO)
	install -Dm644 README.md $(DESTDIR)$(DOCDIR)/README.md
	install -Dm644 LICENSE $(DESTDIR)$(LICENSEDIR)/LICENSE
	install -Dm644 packaging/licenses/nnnoiseless-BSD-3-Clause.txt \
		$(DESTDIR)$(LICENSEDIR)/nnnoiseless-BSD-3-Clause.txt
	install -Dm644 packaging/licenses/THIRD-PARTY-LICENSES.txt \
		$(DESTDIR)$(LICENSEDIR)/THIRD-PARTY-LICENSES.txt
	install -Dm644 vendor/mp3lame-sys/lame-3.100/COPYING \
		$(DESTDIR)$(LICENSEDIR)/LAME-COPYING.txt
	for d in USER_GUIDE CLI API ARCHITECTURE; do \
		install -Dm644 docs/$$d.md $(DESTDIR)$(DOCDIR)/docs/$$d.md; done
	install -d $(DESTDIR)$(DOCDIR)/docs/images
	install -m644 docs/images/*.png $(DESTDIR)$(DOCDIR)/docs/images/
	install -d $(DESTDIR)$(SYSTEMD_USER_DIR)
	sed 's|@BINDIR@|$(BINDIR)|g' packaging/weir.service.in \
		> $(DESTDIR)$(SYSTEMD_USER_DIR)/weir.service
	chmod 644 $(DESTDIR)$(SYSTEMD_USER_DIR)/weir.service
	@if [ -z "$(DESTDIR)" ]; then \
		update-desktop-database "$(APPDIR)" 2>/dev/null || true; \
		touch "$(DATADIR)/icons/hicolor" 2>/dev/null || true; \
		systemctl --user daemon-reload 2>/dev/null || true; \
		echo ""; \
		echo "Weir is installed in $(PREFIX)."; \
		echo "Launch it from your application menu, or run: weir"; \
		echo ""; \
		echo "To start Weir when you log in, tick \"Start Weir when I log in\""; \
		echo "in Preferences, or run: systemctl --user enable weir"; \
		echo ""; \
		case ":$$PATH:" in *":$(BINDIR):"*) ;; *) \
			echo "The weirctl command is in $(BINDIR), which is not on your"; \
			echo "PATH yet. On most systems it is from your next login."; \
			echo "";; \
		esac; \
		if systemctl --user is-active --quiet weir 2>/dev/null; then \
			echo "The daemon is running an older binary. Restart it with:"; \
			echo "  systemctl --user restart weir"; \
			echo ""; \
		fi; \
	fi

uninstall:
	@if [ -z "$(DESTDIR)" ]; then \
		systemctl --user disable --now weir 2>/dev/null || true; \
	fi
	for b in $(BINARIES); do rm -f $(DESTDIR)$(BINDIR)/$$b; done
	rm -f $(DESTDIR)$(APPDIR)/weir.desktop
	rm -f $(DESTDIR)$(ICONDIR)/weir.svg
	rm -f $(DESTDIR)$(SYMBOLIC_ICONDIR)/weir-symbolic.svg
	rm -f $(DESTDIR)$(METAINFODIR)/$(METAINFO)
	rm -f $(DESTDIR)$(SYSTEMD_USER_DIR)/weir.service
	rm -rf $(DESTDIR)$(DOCDIR)
	rm -rf $(DESTDIR)$(LICENSEDIR)
	@if [ -z "$(DESTDIR)" ]; then \
		update-desktop-database "$(APPDIR)" 2>/dev/null || true; \
		systemctl --user daemon-reload 2>/dev/null || true; \
		echo ""; \
		echo "Weir is removed."; \
		echo "Your settings and presets were kept. To delete those too:"; \
		echo "  rm -rf ~/.config/weir"; \
		echo ""; \
	fi

# --- RPM packaging -----------------------------------------------------
# `make rpm` produces a package; `make rpm-install` hands it to dnf. Both
# package the last committed state, so commit before building.

rpm:
	@command -v rpmbuild >/dev/null 2>&1 || { \
		echo "rpmbuild not found. Install the packaging tools with:"; \
		echo "  sudo dnf install rpm-build rpmdevtools desktop-file-utils"; \
		exit 1; }
	@test -d .git || { \
		echo "This is not a git checkout, so the source tarball cannot be built."; \
		echo "Clone the repository instead of downloading a zip, then retry."; \
		exit 1; }
	@spec_version=`sed -n 's/^Version:[[:space:]]*//p' packaging/weir.spec`; \
	if [ "$$spec_version" != "$(VERSION)" ]; then \
		echo "Version mismatch: Cargo.toml is $(VERSION), the spec is $$spec_version."; \
		echo "Make them agree before packaging."; \
		exit 1; fi
	mkdir -p $(RPMTOP)/SOURCES
	git archive --format=tar.gz --prefix=weir-$(VERSION)/ \
		-o $(RPMTOP)/SOURCES/weir-$(VERSION).tar.gz HEAD
	rpmbuild -ba $(RPMBUILD_FLAGS) --define "_topdir $(RPMTOP)" \
		--define "_release $(RPM_RELEASE)" packaging/weir.spec
	@echo ""
	@echo "Package built:"
	@ls -1 $(RPMTOP)/RPMS/*/weir-$(VERSION)-$(RPM_RELEASE)*.rpm

rpm-install: rpm
	@pkg=`ls -1 $(RPMTOP)/RPMS/*/weir-$(VERSION)-$(RPM_RELEASE)*.rpm 2>/dev/null | head -1`; \
	if [ -z "$$pkg" ]; then \
		echo "No package found for release $(RPM_RELEASE). The build above did not"; \
		echo "produce the file this step expected. Please report this."; \
		exit 1; fi; \
	echo "Installing $$pkg"; \
	sudo dnf install -y "$$pkg"
	@echo ""
	@echo "Installed. dnf now owns Weir:"
	@echo "  rpm -q weir        show the installed version"
	@echo "  rpm -ql weir       list every file it owns"
	@echo "  sudo dnf remove weir   uninstall it"

# --- Debian and Ubuntu packaging ---------------------------------------
# `make deb` builds a .deb from the working tree: the files `make install`
# puts down, laid out under /usr. Its library dependencies are worked out
# from the programs themselves, so a package built on the oldest system it
# should support (Ubuntu 24.04) installs on the newer ones too.

DEB_REVISION ?= 1
DEB_ARCH := $(shell dpkg --print-architecture 2>/dev/null)
DEBDIR := target/deb
DEBROOT := $(DEBDIR)/root
DEB := $(DEBDIR)/weir_$(VERSION)-$(DEB_REVISION)_$(DEB_ARCH).deb

deb:
	@command -v dpkg-shlibdeps >/dev/null 2>&1 || { \
		echo "dpkg-shlibdeps not found. Install the packaging tools with:"; \
		echo "  sudo apt install dpkg-dev"; \
		exit 1; }
	rm -rf $(DEBDIR)
	$(MAKE) install DESTDIR=$(CURDIR)/$(DEBROOT) PREFIX=/usr \
		SYSTEMD_USER_DIR=/usr/lib/systemd/user
	@# The release build keeps line numbers for crash reports from builds of
	@# your own; a package leaves them out, which makes it about a fifth the size.
	strip --strip-unneeded $(addprefix $(DEBROOT)/usr/bin/,$(BINARIES))
	@# Debian looks for the license here.
	install -m644 LICENSE $(DEBROOT)/usr/share/doc/weir/copyright
	@# dpkg-shlibdeps insists on a debian/control, even a minimal one.
	mkdir -p $(DEBDIR)/debian $(DEBROOT)/DEBIAN
	printf 'Source: weir\n\nPackage: weir\nArchitecture: any\n' > $(DEBDIR)/debian/control
	cd $(DEBDIR) && dpkg-shlibdeps -O $(addprefix root/usr/bin/,$(BINARIES)) \
		| sed -n 's/^shlibs:Depends=//p' > depends
	test -s $(DEBDIR)/depends
	sed -e 's|@VERSION@|$(VERSION)-$(DEB_REVISION)|' \
		-e 's|@ARCH@|$(DEB_ARCH)|' \
		-e "s|@SIZE@|$$(du -sk $(DEBROOT)/usr | cut -f1)|" \
		-e "s|@DEPENDS@|$$(cat $(DEBDIR)/depends)|" \
		packaging/deb/control.in > $(DEBROOT)/DEBIAN/control
	cd $(DEBROOT) && find usr -type f -exec md5sum {} + | sort -k2 > DEBIAN/md5sums
	dpkg-deb --root-owner-group -Zxz --build $(DEBROOT) $(DEB)
	@echo ""
	@echo "Package built: $(DEB)"

# ---- AppImage ------------------------------------------------------------
#
# One file holding Weir's programs, for any distribution with PipeWire,
# for the computer it is built on (x86_64 or aarch64). `make appimage-dir`
# lays out its contents in target/appimage/AppDir; `make appimage` packs
# them with appimagetool, which it does not install: put it on the PATH or
# point APPIMAGETOOL at it, and point APPIMAGE_RUNTIME at the AppImage
# runtime to put in front, or appimagetool downloads the newest. Both come
# from github.com/AppImage; the Packages workflow uses pinned versions.
#
# The programs need only the C library and PipeWire's library from the
# system, so nothing else is bundled. PipeWire's own library stays the
# system's: it loads its plugins from the system's folders.

APPIMAGE_ARCH := $(shell uname -m)
APPIMAGE_DIR := target/appimage
APPIMAGE_ROOT := $(APPIMAGE_DIR)/AppDir
APPIMAGE_FILE := $(APPIMAGE_DIR)/Weir-$(VERSION)-$(APPIMAGE_ARCH).AppImage
APPIMAGETOOL ?= appimagetool
APPIMAGE_RUNTIME ?=
# Where updaters such as Gear Lever and AppImageUpdate look for the next
# version: the .zsync file beside it on GitHub's latest release.
APPIMAGE_UPDATE := gh-releases-zsync|Mourty|Weir|latest|Weir-*-$(APPIMAGE_ARCH).AppImage.zsync

appimage-dir:
	rm -rf $(APPIMAGE_DIR)
	$(MAKE) install DESTDIR=$(CURDIR)/$(APPIMAGE_ROOT) PREFIX=/usr
	@# Nothing an AppImage uses: the guides are online, and Weir writes its
	@# own login unit, naming the AppImage, when asked to start at login.
	rm -rf $(APPIMAGE_ROOT)/usr/share/doc $(APPIMAGE_ROOT)/usr/share/systemd
	strip --strip-unneeded $(addprefix $(APPIMAGE_ROOT)/usr/bin/,$(BINARIES))
	install -m755 packaging/appimage/AppRun $(APPIMAGE_ROOT)/AppRun
	@# Inside an AppImage the menu entry names the program, not a path.
	sed -i 's|^Exec=.*|Exec=weir|' $(APPIMAGE_ROOT)/usr/share/applications/weir.desktop
	cp $(APPIMAGE_ROOT)/usr/share/applications/weir.desktop $(APPIMAGE_ROOT)/weir.desktop
	cp packaging/weir.svg $(APPIMAGE_ROOT)/weir.svg
	cp crates/gui/assets/weir.png $(APPIMAGE_ROOT)/.DirIcon
	@# The tray names usr/share/icons for the one-color icon. GNOME's tray
	@# looks only at the files right in that folder, Plasma in hicolor.
	cp packaging/weir-symbolic.svg $(APPIMAGE_ROOT)/usr/share/icons/weir-symbolic.svg
	@echo ""
	@echo "Laid out in $(APPIMAGE_ROOT)"

appimage: appimage-dir
	@command -v $(APPIMAGETOOL) >/dev/null 2>&1 || { \
		echo "appimagetool not found. Get it from"; \
		echo "  https://github.com/AppImage/appimagetool/releases"; \
		echo "and run make appimage APPIMAGETOOL=/path/to/appimagetool"; \
		exit 1; }
	@command -v zsyncmake >/dev/null 2>&1 || \
		echo "zsyncmake not found: no .zsync file for updaters (sudo apt install zsync)"
	@# From the AppImage's folder: zsyncmake writes the .zsync file where it runs.
	tool=$$(realpath "$$(command -v $(APPIMAGETOOL))") && cd $(APPIMAGE_DIR) && \
		ARCH=$(APPIMAGE_ARCH) "$$tool" \
		$(if $(APPIMAGE_RUNTIME),--runtime-file $(abspath $(APPIMAGE_RUNTIME))) \
		-u '$(APPIMAGE_UPDATE)' AppDir $(notdir $(APPIMAGE_FILE))
	@echo ""
	@echo "AppImage built: $(APPIMAGE_FILE)"
