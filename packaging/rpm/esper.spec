# Esper, as two RPMs.
#
# **Written and not built here.** This project is developed on Arch, which has
# no `rpmbuild`, so unlike the Arch and Debian packaging this file has never
# produced a package on the machine it was written on. That is stated rather
# than hidden: `scripts/check-packages.sh` checks what it can — that the files
# it claims to install are the ones the manifest names — and cannot check that
# `rpmbuild` accepts it. Building it on a Fedora runner is a release-checklist
# item, and until that has happened this is a best effort by somebody who could
# not run it.
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
* Tue Sep 09 2026 elythi0n <https://github.com/elythi0n/esper> - 0.0.0-1
- Not yet released. This spec has not been built by its author; see the
  comment at the top of the file.
