# MaxNines Desktop for Linux

A tray app for [MaxNines](https://github.com/sfreiberg/maxnines). It shows the alerts
that are firing on your MaxNines server, tells you when they start and resolve, and
links to its web interface. It's the Linux version of MaxNines Desktop for Mac and
Windows, and does the same things.

It runs on Ubuntu 22.04, Debian 12, Fedora, RHEL 10 and its rebuilds such as Rocky and
AlmaLinux, and anything newer. RHEL 9's libraries are too old for it.

## Installing

Download the package for your system from
[Releases](https://github.com/sfreiberg/maxnines-desktop-linux/releases): `x86_64` or
`amd64` for most PCs, `aarch64` or `arm64` for Arm ones.

```sh
sudo apt install ./maxnines-desktop_*.deb     # Debian, Ubuntu, Mint, Pop!_OS
sudo dnf install ./maxnines-desktop-*.rpm     # Fedora, RHEL 10, Rocky, Alma
```

Either one installs what it needs, and adds MaxNines Desktop to your applications.
For anything else, the `.tar.gz` has the program, its desktop entry and icon: put
`maxnines-desktop` on your `PATH`, and the other two in
`~/.local/share/applications` and `~/.local/share/icons/hicolor/scalable/apps`.

## The tray icon

The icon is MaxNines' green 9, with a red dot and how many alerts are firing when
something is critical, or an amber square and how many when there are only warnings.
It turns grey when the server can't be reached, so quiet is never mistaken for fine.
Its menu lists what's firing, and clicking an alert opens its host in your browser.

KDE, Cinnamon, XFCE, MATE, Budgie and Ubuntu's GNOME show tray icons as they come. Other
GNOME desktops, such as Fedora's, need the
[AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).
Without a tray, the app still notifies you, and closing its window quits it.

## Setting it up

1. Open MaxNines Desktop. Its window opens on Settings until it's signed in.
2. Enter the address you open MaxNines at, and choose **Sign in with browser**.
3. Your browser opens MaxNines. Sign in there if you aren't already, choose the role
   the app gets, and choose **Allow**. A viewer sees alerts. A responder can also
   acknowledge and silence them, which is what the app asks for. You can't give it
   more than your own role.
4. The browser says you're signed in, and the window shows your server's alerts.

The app stays signed in for up to 90 days after it was last used. It keeps what it
needs for that in your keyring, through the Secret Service that GNOME Keyring and
KWallet provide. To sign it out, choose **Sign out** in its settings. In MaxNines,
**Settings**, then **Signed-in apps**, lists it as MaxNines Desktop for Linux, with
this computer's name, and you can revoke it there too.

It trusts the certificates your system trusts. A server whose certificate comes from
your company's own certificate authority works once that authority is in the system's
trust store. On Debian and Ubuntu, copy it as a `.crt` file to
`/usr/local/share/ca-certificates` and run `sudo update-ca-certificates`. On Fedora
and RHEL, copy it to `/etc/pki/ca-trust/source/anchors` and run `sudo update-ca-trust`.

Over plain `http`, the sign-in crosses the network unencrypted, so only use it for a
server on a network you trust. Settings warns you under the address when it's plain
`http` to anywhere but this computer.

## What it shows

Its window shows how many hosts are online, the alerts firing, with Acknowledge and
Silence 1 hour, what resolved in the last hour, and the silences running now. If the
server runs checks itself, it also shows how many of them are passing.

## Notifications

The app can tell you when a critical alert starts, when a warning starts, and when an
alert resolves, each turned on or off in its settings. All three are on to begin with.
Alerts that a silence or maintenance window is holding back never send one, as
everywhere else in MaxNines, and one still firing when its silence ends sends one then.
Nothing that was already firing when the app started is announced, and more than three
at once arrive as one.

Each starts with the circle MaxNines uses in Discord: red for critical, yellow for a
warning, and green once it's resolved. A resolved one says where things stand, such as
"web-1 is back online", and how long the problem lasted. A notification about an alert
that's started has Acknowledge, which needs the responder role, and Open in MaxNines.

## Open at login

Turn on **Open at login** in its settings. It then starts in the tray when you sign in,
without opening its window.

## Building

It needs Rust, and GTK 4 and libadwaita's development files:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev     # Debian and Ubuntu
sudo dnf install gtk4-devel libadwaita-devel       # Fedora

cargo build --release      # makes target/release/maxnines-desktop
cargo test --workspace     # runs the tests
```

The `kit` crate holds everything that doesn't need a desktop: the API, signing in, and
working out what's worth a notification. Its tests run anywhere, macOS included.

## License

MIT. See [LICENSE](LICENSE).
