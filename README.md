# Nexus-Way

Nexus-Way contains the HIVE self-hosted backend and the Nexus Connect Android application.

This repository is source-available for authorized security research. It is not open source. See [License.md](License.md), [SECURITY.md](SECURITY.md), and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Repository Map

- `services/hive/` - HIVE server, migrations, legal text, tests, and a neutral configuration example.
- `apps/android/connect/` - Nexus Connect and Nexus Notify Android applications.
- `packages/rust/hive-client/` - shared authenticated HIVE protocol client.
- `packages/rust/nexus-common/` - shared cryptographic and IPC primitives.
- `web/site/` - public signup, login, and authenticated application download site.

## Validation

```bash
cargo test --workspace

cd apps/android/connect/android
./gradlew test lintDebug assembleDebug
```

Production releases additionally require signer, artifact hash, authenticated endpoint, notification, and physical-device checks.

## Security

Do not open public issues containing vulnerabilities, credentials, private data, or exploit details. Follow [SECURITY.md](SECURITY.md).
