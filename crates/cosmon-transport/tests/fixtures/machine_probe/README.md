# `machine_probe` darwin fixtures

Captured verbatim from a real darwin host (Darwin 25.6.0, arm64, 128 GiB) on
2026-09-06 by running the command each filename names. They are the *oracle*
for `cosmon_transport::machine_probe`'s parsers: the expected byte values in
the unit tests are computed by hand from these bytes, never copied out of the
code under test, so a parser rewritten to agree with itself still fails.

`vm_swapusage_busy.txt` is the one file not captured on this host — it is the
same format with a non-zero, non-integral reading, because the host that could
be captured has swap disabled and a fixture of all zeroes cannot catch a
unit mistake (0 MiB and 0 bytes are the same number).
