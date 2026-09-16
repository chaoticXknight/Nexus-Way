# Reminder: Two-Phone Connect Test

## September 15, 2026: Production Release Preparation

- [x] User approved the tested changes for commit and server publication, using
      their configured Git author/committer identity without AI co-authorship.
- [x] Update Connect release notes to 0.2.51. Rust workspace tests and optimized
      HIVE build passed. Android unit tests, Connect/Notify debug lint, and signed
      production release builds passed without localReliabilityTest/callDeviceTests.
      Verified Connect 0.2.51 (2051), Notify 0.2.36 (2036), and both APK signatures
      against the existing release signer. Earlier local phone checks remain
      recorded below; the production APKs have not been installed on the phones.
- [ ] Deploy HIVE with migration 0025 and publish both production APKs plus release
      notes. SSH key login succeeded and HIVE is active, but non-interactive sudo
      requires interactive authentication. No live server files were changed.
- [ ] After deployment, verify migration 0025, service health, published artifact
      hashes, release metadata, and authenticated download/version endpoints.

## September 15, 2026: Call Notification and Phone-Lock Follow-Up

- [x] Resolve the verified caller profile before posting the first incoming-call
      alert so the heads-up notification and full-screen view start with the same
      name. Lookup waits at most three seconds, then uses the verified local
      handle/account fallback. Active ringing notifications remain untouched.
      Unit tests, lint, and optimized release build passed; installed in place
      on the daily T705M phone and relaunched Connect.
- [ ] Verify caller identity in the unlocked heads-up alert during a real call;
      install the caller-name follow-up on Mom's phone as well.

- [x] Stop incoming calls from automatically replacing Connect's current page
      with CallPane while Android already shows the heads-up Answer/Decline alert.
      Outgoing/answered calls and explicit fallback-notification taps still open
      call controls; locked-screen notification presentation is unchanged.
      Unit tests and optimized release build passed; installed in place on the
      daily T705M phone and relaunched Connect.
- [ ] Install this latest heads-up-only UI follow-up on Mom's phone and verify
      unlocked incoming calls leave the current page visible on both phones.

- [x] Follow-up ringtone regression reproduced on Mom's Android 12 phone:
      updating an active looping call notification stopped Android's sound.
      Preserve the existing ringing notification while updating caller names in
      the live call screen. The actual system-ringtone regression failed before
      the fix and passed afterward. Unit tests, lint, and optimized build passed;
      installed the optimized fix on Mom's phone and relaunched Connect.
- [x] Install the latest optimized ringtone follow-up on the daily T705M phone
      in place and relaunch Connect. Background use remains allowed and Connect
      remains Doze-exempt. Both phones now have the final ringtone fix for testing.
- [x] Review pending publication scope: Connect/Notify Android source, tests,
      build settings, HIVE pending-call replay code/tests/migration, and this progress
      document. No files are staged. APKs, saved phone artifacts, logs, runtime
      databases, and signing keys are excluded by existing ignore rules. A targeted
      scan of pending files found no obvious embedded credentials, phone serials,
      personal absolute paths, or private NEXUS-APPS code references. This is a
      publication-hygiene check, not a new full behavioral review of earlier changes.
      Keep migration 0025 with the HIVE changes; publish using normal release
      settings, without localReliabilityTest or callDeviceTests. Nothing committed,
      pushed, or deployed.
- [ ] Investigate the friend's comment-edit report after clarifying whether Edit
      is missing or Save fails. Android has author-only Edit controls and a server
      edit endpoint; no speculative authorization or comment behavior change made.

- [x] Read the daily T705M phone's logs and confirmed WebRTC null-input crashes
      during speaker routing and call cleanup. Select actual input microphones
      instead of passing null or an output device to WebRTC.
- [x] Replace the silent incoming-call notification with Android CallStyle,
      ringtone, vibration, caller identity, full-screen intent, Answer, and Decline.
      Profile identity is matched to the authenticated sender, with a verified
      message-history handle fallback. Retries preserve the invite expiry.
- [x] Add a call-only activity above the keyguard, sharing the existing call UI
      and service-owned media. Notification actions validate the original call ID;
      Answer waits until the activity is resumed before starting microphone capture.
- [x] Add Android notification, full-screen, and battery-access links under
      Connect Settings -> Messaging. Normal notification cleanup preserves ringing calls.
- [x] Run 22 JVM tests, Android debug lint, and the optimized signed release build.
- [x] Run 3 native device tests on the signed, unshrunk local test build: sustained
      screen-off call popup with caller/controls and Decline, direct notification
      Answer while locked, and real WebRTC speaker/hangup crash regressions.
      The first ActivityScenario-based test covered the call with its own blank
      activity; the replacement exercises Android's full-screen notification alone.
- [x] User enabled full-screen-call access and background battery use, then approved
      Android's separate Doze exemption. Verified RUN_ANY_IN_BACKGROUND=allow and
      Connect's presence in the device-idle exemption list after final installation.
- [x] Install the normal optimized signed 0.2.51-local build in place, remove only
      the temporary instrumentation APK, and relaunch Connect successfully. The APK
      signing certificate matched the existing installation; no account data was cleared.
- [x] Install the same optimized Connect 0.2.51-local update and Notify
      0.2.36-local on Mom's Moto G Stylus 5G (Android 12). Both signing
      certificates matched; in-place installation preserved account data.
      Her pre-update 0.2.50 logs confirmed the same WebRTC null-input crash
      when answering. Microphone, camera, and full-screen permissions are
      granted; the new call channel has high importance, ringtone, and vibration.
      Background use is allowed and Connect is already Doze-exempt. Connect
      relaunched without a new observed startup crash, and Notify is running.
      Pre-update APKs are in ignored `build/phone-verification-20260915-mom/`.
- [ ] Verify real calls in both directions, remote caller names, audible ringing,
      Answer/Decline, and two-way audio with each phone locked for at least 15 minutes.
      Local synthetic tests do not prove remote delivery or sustained media continuity.

Build from `apps/android/connect/android/` with
`ANDROID_HOME=$HOME/Android/Sdk ./gradlew :app:testDebugUnitTest :app:lintDebug :app:assembleRelease -PlocalReliabilityTest=true`.
For native tests, build `:app:assembleRelease :app:assembleReleaseAndroidTest` with
`-PlocalReliabilityTest=true -PcallDeviceTests=true`, install both APKs in place,
and run `adb shell am instrument -w -r -e class com.nexusway.connect.CallDeviceTest com.nexusway.connect.test/androidx.test.runner.AndroidJUnitRunner`.
Leave the phone untouched during synthetic calls. Restore the optimized build afterward.
The pre-update APK is kept in ignored `build/phone-verification-20260915/`;
it is not an account-data backup. No GitHub push or Hive deployment was performed.

## Tomorrow: September 12, 2026

- [x] Connect Mom's phone by USB and install both signed local test APKs without
      uninstalling the existing apps or clearing account data.
- [x] Install Connect 0.2.51-local and Nexus Notify 0.2.36-local, matching the daily phone.
- [ ] Do not use the server-offered Notify update: the hash-based indicator still
      flags the different local test build. Automatic updates are disabled in it.
- [ ] Reconnect the daily phone and restart filtered log capture before testing.
- [ ] Test voice calls both ways, lock each phone for at least 15 minutes, and
      verify two-way audio, hangup, mute, speaker, and network-change recovery.
- [ ] Test message notifications while backgrounded and screen-off; record delays
      and battery restrictions. Neither app was battery-optimization exempt on
      the daily phone; those settings were not changed.
- [ ] Confirm ordinary messaging, photo/video attachments, and app startup still work.
- [ ] Review logs and approve the changes before any GitHub push.

## Local Artifacts

Relative to `apps/android/connect/android/`:

- Connect: `app/build/outputs/apk/release/app-release.apk`
- Notify: `notifier/build/outputs/apk/release/notifier-release.apk`
- Captured log: `build/phone-verification-20260911/device.log`
- Original installed APKs: `build/phone-verification-20260911/`

Original APKs are not an account-data backup. Do not uninstall to downgrade.
Keep captured phone logs out of Git.

## Completed Tonight

- Both signed updates installed in place on the daily T705M phone (Android 15).
- Signing certificates matched; notification, microphone, and camera permissions remained granted.
- Connect launched and Notify authenticated with Hive; no new startup crash was observed.
- 17 Android tests and 39 Hive tests passed; Android lint and signed release builds passed.
- Physical two-phone verification is still pending. No GitHub push or live Hive
  deployment was performed. Server-side pending-invite replay needs the separately
  approved Hive deployment before it can be tested against the live server.