# Battmon's RPM spec. Builds offline from a source tarball and a vendor
# tarball, both made by packaging/rpm/make-vendor-tarball.sh (see README.md
# there). The version is Cargo.toml's; %prep stops if this file disagrees.

%global debug_package %{nil}
%global app_id design.beben.Battmon

Name:           battmon
Version:        0.1.0
Release:        1%{?dist}
Summary:        Battery monitor for GNOME
License:        MIT
URL:            https://github.com/luishowin/battmon
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo >= 1.92
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4) >= 4.24
BuildRequires:  pkgconfig(libadwaita-1) >= 1.10
BuildRequires:  pkgconfig(sqlite3)
BuildRequires:  systemd-rpm-macros
BuildRequires:  desktop-file-utils

%description
Battmon is a small, native battery monitor for GNOME: a tiny background sampler
plus a GTK4/libadwaita window. It reads /sys/class/power_supply directly — no
UPower, no D-Bus, no web stack.

It shows the live level, power draw, charge rate, time left and time to full,
battery health with a daily trend, cycle count, and level history for the last
24 hours and 7 days. The battmond user service collects the history.

%prep
%autosetup -p1
tar -xf %{SOURCE1}
mkdir -p .cargo
cat vendor-config.toml >> .cargo/config.toml
grep -q '^version = "%{version}"$' Cargo.toml

%build
cargo build --release --locked --offline

%check
cargo test --release --locked --offline

%install
install -Dm0755 target/release/battmon %{buildroot}%{_bindir}/battmon
install -Dm0755 target/release/battmond %{buildroot}%{_bindir}/battmond
install -Dm0644 data/%{app_id}.desktop \
    %{buildroot}%{_datadir}/applications/%{app_id}.desktop
install -Dm0644 data/%{app_id}.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
install -d %{buildroot}%{_userunitdir}
sed 's|^ExecStart=.*|ExecStart=%{_bindir}/battmond|' data/battmond.service \
    > %{buildroot}%{_userunitdir}/battmond.service
chmod 0644 %{buildroot}%{_userunitdir}/battmond.service
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop

%files
%license LICENSE
%doc README.md
%{_bindir}/battmon
%{_bindir}/battmond
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_userunitdir}/battmond.service

%changelog
* Thu Nov 19 2026 Luis Maina <luis.howin.maina@gmail.com> - 0.1.0-1
- First Battmon RPM: GTK4 window, systemd user sampler, desktop entry, icon.
- Power card turns green while the machine is on wall power.
