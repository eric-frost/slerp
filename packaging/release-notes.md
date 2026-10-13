The first release of Slerp, a thumbnailer and viewer for 3D models, textures, game files and other graphics files that desktops usually show as generic icons.  It is young and has one developer, and for many of its 200 or so formats it reads only part of what the format can hold, so expect files it gets wrong.  Bug reports are welcome, most of all with a file you're free to share.

The files below were built from this release's source by the repository's GitHub workflow.  They aren't signed with a certificate, so Windows and macOS warn about them the first time; the steps for each are below.

## Windows 10 and 11 (x64)

Download `Slerp-Setup-windows-x64.exe` and run it.  It installs Slerp for your account only, without administrator rights, sets up Explorer's thumbnails and the "Open with" entry, and adds the viewer to the Start menu.  Settings, Apps, Installed apps removes it again.

Because the installer isn't signed, SmartScreen says "Windows protected your PC".  Click "More info", then "Run anyway".  If Smart App Control is switched on, Windows blocks unsigned programs outright, and Slerp can't be installed.

`Slerp-windows-x64.zip` holds the same programs without an installer.  Its `README.txt` says how to set them up by hand.

## macOS 11 or newer (Apple Silicon)

Download `Slerp-macos-arm64.dmg`, open it and drag Slerp to Applications.  The app is signed ad hoc but not notarized by Apple, so macOS blocks it the first time you open it.  Then open System Settings, Privacy & Security, click "Open Anyway" beside the message about Slerp, and confirm.  (macOS 15 removed the older way, Control-clicking the app and choosing Open.)

Or install it from Terminal, which avoids the warning, since curl doesn't mark what it downloads as coming from the internet:

```
curl -fL -o /tmp/Slerp.zip \
  https://github.com/eric-frost/slerp/releases/latest/download/Slerp-macos-arm64.zip &&
  rm -rf /Applications/Slerp.app && ditto -x -k /tmp/Slerp.zip /Applications &&
  open /Applications/Slerp.app
```

The same command updates Slerp later: it replaces the app that is there.

Finder shows Slerp's thumbnails once the app has been opened.  Intel Macs aren't supported.

## Linux (x86_64 and arm64)

Each distribution gets two packages: `slerp`, and `slerp-qt-plugin`, which is optional and lets Qt programs such as Gwenview and Dolphin's Information panel show Slerp's files.  The names below are for x86_64; on arm64 they end in `arm64.deb`, `aarch64.rpm` and `aarch64.tar.gz`.

- Debian 12 or newer, Ubuntu 22.04 or newer: `sudo apt install ./slerp_0.1.0-1_amd64.deb ./slerp-qt-plugin_0.1.0-1_amd64.deb`
- Fedora: `sudo dnf install ./slerp-0.1.0-1.x86_64.rpm ./slerp-qt-plugin-0.1.0-1.x86_64.rpm`
- Arch Linux (x86_64 only): `sudo pacman -U slerp-0.1.0-1-x86_64.pkg.tar.zst slerp-qt-plugin-0.1.0-1-x86_64.pkg.tar.zst`
- Other distributions: unpack `slerp-0.1.0-linux-x86_64.tar.gz` and run `sudo ./install.sh` in it, which installs under `/usr/local`.  GNOME's file managers need `sudo ./install.sh --prefix /usr`, because their thumbnail sandbox sees only `/usr`.  `--uninstall` removes it again.

Most file managers pick up the thumbnails by themselves.  Dolphin keeps its own list, so each user who wants them there runs `slerp associate --apply` once and restarts Dolphin.

## Checking the downloads

`SHA256SUMS` lists the checksum of each file; `sha256sum -c SHA256SUMS --ignore-missing` checks the ones you downloaded.  Each file also has a build attestation, which shows that it was built from this release's commit by this repository's workflow.  With the GitHub command line tool:

```
gh attestation verify Slerp-Setup-windows-x64.exe --repo eric-frost/slerp
```
