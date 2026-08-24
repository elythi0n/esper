# Esper, as two RPMs.
#
# **Built, in a Fedora, by `make check-rpm`.** For most of its life this file
# carried a paragraph admitting the opposite: this project is developed on
# Arch, which has no `rpmbuild`, so the spec had never produced a package and
# `scripts/check-packages.sh` could only compare its `%files` list against the
# manifest. Saying so was honest and was not the same as checking.
#
# The first time it met `rpmbuild`, three things were wrong that no amount of
# reading had found — `BuildRequires` was short a C++ compiler, a Go test
# borrowed `XDG_RUNTIME_DIR` from a logged-in session, and a Rust test assumed
# two statements run within 20 ms of each other. All three are the same
# mistake: something true of the machine it was written on, written down as
# though it were true of the software.
#
# `make check-rpm` builds this in a pinned Fedora container, runs the project's
# suite in `%check`, then installs what comes out on a *clean* Fedora and has
# systemd read the units. It compiles the whole workspace, so it is tens of
# minutes and is not part of `make all`.
#
# `%install` calls the same script the other two formats call, so the files
# that land in the buildroot cannot drift from them. `%files` is the exception:
# RPM requires every path to be named, and a package that installs a file it
# does not list fails to build. So that list *is* duplication, and
# `scripts/check-packages.sh` compares it against the manifest rather than
# trusting it.

%global _esper_root %{_sourcedir}/esper

Name:           esper
Version:        %{?_esper_version}%{!?_esper_version:0.0.0}
Release:        1%{?dist}
Summary:        Secure remote desktop and terminal, with no operated infrastructure

License:        MIT
URL:            https://github.com/elythi0n/esper
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  golang >= 1.26
BuildRequires:  cargo >= 1.98
BuildRequires:  rust >= 1.98
BuildRequires:  make
BuildRequires:  cmake
BuildRequires:  nasm
# `openh264-sys2` builds the codec from C++ source. Fedora's build root has a C
# compiler and no C++ one, so without this the build reaches the video crate
# after several minutes of compiling everything else and stops with
# `failed to find tool "g++"`. Found by building it.
BuildRequires:  gcc-c++
Requires:       systemd

%description
Esper gives you your own machine from somewhere else: its screen, its keyboard,
its shell and its ports. Sessions are end-to-end encrypted between two devices
that have been paired with each other, and no part of it depends on a server
the project runs.

# The broker is a separate package because a different person installs it, on a
# different machine, for a different reason — see the Arch packaging, which
# splits for the same reason and says so at more length.
%package -n esper-broker
Summary:        Introduces two Esper peers to each other; holds no session keys
Requires:       systemd

%description -n esper-broker
A broker helps two machines find each other when neither can be dialled
directly. It holds no session keys and decrypts nothing: what it introduces is
already encrypted between the two ends.

Install this only on a server you want to act as an introducer. It is not
needed to use Esper.

%prep
%autosetup

%build
# SOURCE_DATE_EPOCH is set by rpmbuild from the changelog date, and the
# Makefile stamps both halves of the build from it.
make build

%install
./scripts/package-files.sh esper %{buildroot} rpm build
./scripts/package-files.sh esper-broker %{buildroot} rpm build

%check
make test

%files
%license LICENSE
%{_bindir}/esper
%{_bindir}/esper-engine
%{_prefix}/lib/systemd/user/esper.service

%files -n esper-broker
%license LICENSE
%{_bindir}/esper-broker
%{_prefix}/lib/systemd/system/esper-broker.service

%changelog
* Wed Sep 09 2026 elythi0n <https://github.com/elythi0n/esper> - 0.0.0-1
- Not yet released. Built and installed in a Fedora container by
  `make check-rpm`; see the comment at the top of the file.
