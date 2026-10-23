Name:           openconverter
Version:        3.0.1
Release:        1%{?dist}
Summary:        Offline file converter for documents, images, audio, video, fonts and archives
License:        MIT
URL:            https://github.com/Yusuf6411/OpenConverter
Source0:        %{name}-%{version}.tar.gz
# Produced by tools/vendor_source.sh: the sources plus a vendor/ directory, so the
# build needs no network access (COPR builds run offline).
BuildRequires:  cargo
BuildRequires:  rust

%description
OpenConverter converts between many file formats using its own Rust code for every
format. It ships a command line, a local web interface and Python bindings.

%prep
%autosetup -n %{name}-%{version}
mkdir -p .cargo
cat > .cargo/config.toml <<CFG
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CFG

%build
cargo build --release --offline -p oc-cli

%install
install -Dm755 target/release/openconverter %{buildroot}%{_bindir}/openconverter

%check
cargo test --release --offline -p oc-cli

%files
%license LICENSE
%doc README.md
%{_bindir}/openconverter

%changelog
* Fri Oct 23 2026 OpenConverter maintainers <noreply@example.invalid> - 3.0.1-1
- Initial package
