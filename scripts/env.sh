# Edit for your setup, or export the variables before sourcing. Sourced by the run scripts.
# Nothing here is a secret. The key file is created by you (mode 0600) and is not in git.
#
#   PORT   RP2040 device serial port. Linux: /dev/ttyACM0. macOS: /dev/cu.usbmodem*
#   PI     ssh host of the Pi (an alias in ~/.ssh/config is convenient)
#   PI_DIR directory on the Pi that holds moq-subghz-send and moq-subghz-recv
#   LKEY   key path on the laptop (bench.sh). PKEY is the same file's path on the Pi.
: "${PORT:=/dev/ttyACM0}"
: "${PI:=pi}"
: "${PI_DIR:=moq-subghz}"
: "${BIN:=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release}"
: "${RUNS:=$(pwd)/runs}"
mkdir -p "$RUNS"
