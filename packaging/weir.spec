# RPM package definition for Fedora and Nobara.
#
# The easy way to build and install this is from the top of the checkout:
#
#   make rpm-install
#
# which creates the source tarball, builds the package and hands it to dnf.
# See the README for the prerequisites. To build the package without
# installing it, run `make rpm`.
#
# Note: this builds with plain cargo, so it downloads crates from the network
# during the build. That works with rpmbuild on your own machine, in GitHub's
# build containers, and on COPR with "internet access during builds" on.
# Offline build services need a `cargo vendor` tarball as a second Source.

# Fall back gracefully when systemd-rpm-macros or the AppStream macros are
# not installed.
%{!?_userunitdir: %global _userunitdir %{_prefix}/lib/systemd/user}
%{!?_metainfodir: %global _metainfodir %{_datadir}/metainfo}

# The release profile is built without debug info, so there is no debuginfo to
# extract into a subpackage. Fedora's own rpm macros export RUSTFLAGS
# containing -Cdebuginfo=2, so this has to be set explicitly, and %build,
# %install and %check must pass exactly the same cargo settings. If they
# differ, cargo sees a different fingerprint and rebuilds the whole tree
# again, which on GitHub's builders costs a quarter of an hour.
%global debug_package %{nil}
%global cargo_env CARGO_PROFILE_RELEASE_DEBUG=false
%global cargo_flags --release --locked

Name:           weir
Version:        1.0.2
Release:        %{?_release}%{!?_release:1}%{?dist}
Summary:        Voicemeeter-style audio mixer for PipeWire

# Weir's own license. The libraries built into it are listed, with their
# licenses, in THIRD-PARTY-LICENSES.txt.
License:        MIT
URL:            https://github.com/Mourty/Weir
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  clang
BuildRequires:  make
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

Requires:       pipewire
Requires:       wireplumber

%description
Weir is a mixer for PipeWire in the style of Voicemeeter. It presents
vertical input strips with faders, peak meters, mute, solo, pan and per-bus
routing buttons, and output buses that are either real devices or virtual
microphones that other applications can capture from.

It installs three programs: weir-daemon (the mixing daemon), weir (the mixer
window) and weirctl (a command line client that exposes the same control API
for scripts and stream decks).

%prep
%autosetup -n weir-%{version}

%build
%{cargo_env} make %{?_smp_mflags} CARGO_FLAGS="%{cargo_flags}"

%install
%{cargo_env} make install \
    CARGO_FLAGS="%{cargo_flags}" \
    DESTDIR=%{buildroot} \
    PREFIX=%{_prefix} \
    SYSTEMD_USER_DIR=%{_userunitdir}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.github.mourty.weir.metainfo.xml
%{cargo_env} cargo test --workspace %{cargo_flags}

%files
%license %{_datadir}/licenses/weir/LICENSE
%license %{_datadir}/licenses/weir/nnnoiseless-BSD-3-Clause.txt
%license %{_datadir}/licenses/weir/THIRD-PARTY-LICENSES.txt
%{_bindir}/weir
%{_bindir}/weir-daemon
%{_bindir}/weirctl
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{name}-symbolic.svg
%{_metainfodir}/io.github.mourty.weir.metainfo.xml
%{_userunitdir}/weir.service
%dir %{_datadir}/doc/%{name}
%doc %{_datadir}/doc/%{name}/README.md
%doc %{_datadir}/doc/%{name}/docs/

%changelog
* Wed Sep 30 2026 Mourty <Mourt2@proton.me> - 1.0.2-1
- Show the tray icon and open the window when Weir starts at login before
  the desktop is ready.
- Keep the window from freezing while it is minimized, which stopped it
  closing at shutdown.
- Keep to one mixer window: starting Weir again brings the open one forward.

* Wed Sep 30 2026 Mourty <Mourt2@proton.me> - 1.0.1-1
- Add a package for Arch Linux and builds on COPR for Fedora and Nobara.
  Weir itself is unchanged.

* Wed Sep 30 2026 Mourty <Mourt2@proton.me> - 1.0.0-1
- First release.
