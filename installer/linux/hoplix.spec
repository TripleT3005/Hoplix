%global debug_package %{nil}

Name: hoplix
Version: APP_VERSION
Release: 1
Summary: File transfer client for SFTP and FTP(S)
License: MIT
URL: https://github.com/TripleT3005/Hoplix
Requires: libxkbcommon.so.0()(64bit)
Requires: libwayland-client.so.0()(64bit)
Requires: libX11.so.6()(64bit)
Requires: libX11-xcb.so.1()(64bit)
Requires: libXcursor.so.1()(64bit)
Requires: libXi.so.6()(64bit)
Requires: libxkbcommon-x11.so.0()(64bit)
Recommends: libEGL.so.1()(64bit)
Recommends: libwayland-egl.so.1()(64bit)

%description
Desktop app and command line tool to browse servers, transfer files with
a queue, compare and synchronize folders, and manage saved sites.

%install
cp -a %{_sourcedir}/root/. %{buildroot}/

%files
%{_bindir}/hoplix
%{_bindir}/hoplix-cli
%{_datadir}/hoplix
%{_datadir}/applications/hoplix.desktop
%{_datadir}/icons/hicolor/*/apps/hoplix.*
%{_docdir}/hoplix
