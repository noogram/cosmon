<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Sign the macOS daemon supervisor

macOS can attribute a supervised child's App Data or Full Disk Access request
to its responsible launchd job. Give access to the installed
`~/.local/bin/cosmon-daemon-supervisor` only after it has a stable signing
requirement. The identifier is `com.cosmon.daemon-supervisor`.

1. On the operator's Mac, run `scripts/bootstrap-supervisor-signing-cert.sh`
   once. It creates the `Cosmon Local Signing` certificate in the login
   keychain. Keep that certificate and private key: replacing them changes
   the designated requirement and can invalidate an existing grant.
2. From the integrated trunk, run `just install`. The supervisor binary's
   install path copies and signs it. This does not restart the loaded
   LaunchAgent. To install a previously absent LaunchAgent, the operator can
   separately run `scripts/install-daemon-supervisor.sh install`.
3. Run `cs doctor supervision`. It warns if the installed binary has a
   content-derived identifier, an ad-hoc content-hash requirement, or an
   unreadable signature. Inspect it directly with
   `codesign -dv ~/.local/bin/cosmon-daemon-supervisor` and
   `codesign -dr - ~/.local/bin/cosmon-daemon-supervisor`.
4. In macOS System Settings, add the **installed supervisor binary** to
   Privacy & Security → Full Disk Access and enable it once. The install
   scripts and doctor never change TCC settings. The operator decides when
   to restart the supervisor so the running process takes up the grant.

If the certificate is unavailable, the installer still pins the identifier
but signs ad-hoc and warns. That signature's designated requirement contains
a content hash, so the Full Disk Access grant cannot be expected to survive
the next rebuild. Set `COSMON_SUPERVISOR_SIGNING_IDENTITY` consistently if
using a different persistent signing certificate.
