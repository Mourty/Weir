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
# during the build. That works with rpmbuild on your own machine. Offline
# build services such as COPR need a `cargo vendor` tarball as a second Source
# or the %%cargo_* macros from rust-packaging.

# Fall back gracefully when systemd-rpm-macros is not installed.
%{!?_userunitdir: %global _userunitdir %{_prefix}/lib/systemd/user}

# The release profile is built without debug info, so there is no debuginfo to
# extract into a subpackage. Fedora's own rpm macros export RUSTFLAGS
# containing -Cdebuginfo=2, so this has to be set explicitly, and %build and
# %install must pass exactly the same cargo settings. If they differ, cargo
# sees a different fingerprint and rebuilds the whole tree a second time
# during %install.
%global debug_package %{nil}
%global cargo_env CARGO_PROFILE_RELEASE_DEBUG=false
%global cargo_flags --release --locked

Name:           weir
Version:        1.0.0
Release:        %{?_release}%{!?_release:1}%{?dist}
Summary:        Voicemeeter-style audio mixer for PipeWire

License:        MIT
URL:            https://github.com/Mourty/Weir
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  clang
BuildRequires:  make
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  desktop-file-utils

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
cargo test --workspace --release --locked

%files
%license %{_datadir}/licenses/weir/LICENSE
%license %{_datadir}/licenses/weir/nnnoiseless-BSD-3-Clause.txt
%{_bindir}/weir
%{_bindir}/weir-daemon
%{_bindir}/weirctl
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{name}-symbolic.svg
%{_userunitdir}/weir.service
%dir %{_datadir}/doc/%{name}
%doc %{_datadir}/doc/%{name}/README.md
%doc %{_datadir}/doc/%{name}/docs/

%changelog
* Wed Sep 30 2026 Mourty <Mourt2@proton.me> - 1.0.0-1
- First release.
