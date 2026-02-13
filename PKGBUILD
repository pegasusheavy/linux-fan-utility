# Maintainer: Joseph Quinn <quinn.josephr@proton.me>
pkgname=fanctl
pkgver=0.1.0
pkgrel=1
pkgdesc="A Linux fan control daemon and TUI client using hwmon sysfs"
arch=('x86_64')
url="https://github.com/PegasusHeavyIndustries/linux-fan-utility"
license=('MIT')
depends=('gcc-libs')
makedepends=('cargo')
source=()

build() {
  cd "$startdir"
  cargo build --release --locked
}

package() {
  cd "$startdir"

  # Binaries
  install -Dm755 "target/release/fanctl-daemon" "$pkgdir/usr/bin/fanctl-daemon"
  install -Dm755 "target/release/fanctl-tui" "$pkgdir/usr/bin/fanctl-tui"

  # systemd unit
  install -Dm644 "fanctl-daemon.service" "$pkgdir/usr/lib/systemd/system/fanctl-daemon.service"

  # Default config directory
  install -dm755 "$pkgdir/etc/fanctl"

  # License
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
