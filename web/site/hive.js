// Owns the website's same-origin HIVE protocol, browser cryptography, and sessionStorage state.
// Page scripts own forms and navigation; vendored libraries own Argon2 and Ed25519 primitives.
// Browser HIVE client — the JS twin of HiveClient.kt / hive-client/src/lib.rs.
// Speaks the exact wire protocol:
//   device cert:  "hive-device-cert:v1:{device_pub_b64}:{name}:{created}"
//   auth finish:  "hive-auth:v1:{challenge}:{server_pub_b64}:{timestamp}"
//   escrow blob:  [16B salt][12B nonce][AES-256-GCM(identity seed)]
//                 key = Argon2id(password, salt, m=19456KB, t=2, p=1)
// All crypto happens in the browser; the server only ever sees ciphertext.
// Requires vendor/argon2.umd.min.js (hash-wasm, global `hashwasm`) loaded first.

import { getPublicKeyAsync, signAsync } from './vendor/ed25519.js';

const enc = new TextEncoder();

export function b64(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

export function unb64(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function sha256hex(bytes) {
  const d = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return [...d].map((b) => b.toString(16).padStart(2, '0')).join('');
}

async function api(path, body, token) {
  const headers = { 'Content-Type': 'application/json' };
  if (token) headers['Authorization'] = 'Bearer ' + token;
  const resp = await fetch(path, {
    method: 'POST',
    headers,
    body: JSON.stringify(body),
  });
  const v = await resp.json();
  if (!v.ok) throw new Error(v.err || 'server error');
  return v;
}

// ------------------------------------------------------------- escrow

async function deriveKey(password, salt) {
  const raw = await hashwasm.argon2id({
    password,
    salt,
    parallelism: 1,
    iterations: 2,
    memorySize: 19456,
    hashLength: 32,
    outputType: 'binary',
  });
  return crypto.subtle.importKey('raw', raw, 'AES-GCM', false, [
    'encrypt',
    'decrypt',
  ]);
}

async function sealEscrow(password, seed) {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const key = await deriveKey(password, salt);
  const ct = new Uint8Array(
    await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce }, key, seed)
  );
  const blob = new Uint8Array(16 + 12 + ct.length);
  blob.set(salt, 0);
  blob.set(nonce, 16);
  blob.set(ct, 28);
  return blob;
}

async function openEscrow(password, blob) {
  if (blob.length < 16 + 12 + 16) throw new Error('corrupt recovery data');
  const key = await deriveKey(password, blob.slice(0, 16));
  try {
    return new Uint8Array(
      await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv: blob.slice(16, 28) },
        key,
        blob.slice(28)
      )
    );
  } catch {
    throw new Error('wrong handle or password');
  }
}

// --------------------------------------------------------------- auth

async function serverPub() {
  const v = await (await fetch('/v1/info')).json();
  if (!v.ok) throw new Error('server unavailable');
  return v.server_pub;
}

async function authWith(accountId, deviceSeed) {
  const devicePub = await getPublicKeyAsync(deviceSeed);
  const deviceId = await sha256hex(devicePub);
  const begin = await api('/v1/identity/auth_begin', {
    account_id: accountId,
    device_id: deviceId,
  });
  const pub = await serverPub();
  const ts = Math.floor(Date.now() / 1000);
  const msg = `hive-auth:v1:${begin.challenge}:${pub}:${ts}`;
  const sig = b64(await signAsync(enc.encode(msg), deviceSeed));
  const fin = await api('/v1/identity/auth_finish', {
    account_id: accountId,
    device_id: deviceId,
    challenge: begin.challenge,
    timestamp: ts,
    sig,
  });
  return fin.token;
}

async function deviceCert(identitySeed, devicePub, name, created) {
  const msg = `hive-device-cert:v1:${b64(devicePub)}:${name}:${created}`;
  return b64(await signAsync(enc.encode(msg), identitySeed));
}

// ------------------------------------------------------------ flows

/** Create a Connect account: register + sign in + park the recovery
 *  bundle. Afterwards the Android app signs in with handle + password. */
export async function signUp({ invite, handle, password }) {
  const identitySeed = crypto.getRandomValues(new Uint8Array(32));
  const deviceSeed = crypto.getRandomValues(new Uint8Array(32));
  const identityPub = await getPublicKeyAsync(identitySeed);
  const devicePub = await getPublicKeyAsync(deviceSeed);
  const created = Math.floor(Date.now() / 1000);
  const name = 'Web signup';
  const reg = await api('/v1/identity/register', {
    identity_pub: b64(identityPub),
    handle,
    invite_code: invite || null,
    device_pub: b64(devicePub),
    device_name: name,
    device_created: created,
    device_cert: await deviceCert(identitySeed, devicePub, name, created),
    // Age gate: the sign-up form requires the 16-or-older attestation
    // before this code is reachable.
    age_confirmed: true,
  });
  const token = await authWith(reg.account_id, deviceSeed);
  await api(
    '/v1/identity/escrow_set',
    { path: 'password', blob: b64(await sealEscrow(password, identitySeed)) },
    token
  );
  sessionStorage.setItem('hive_token', token);
  sessionStorage.setItem('hive_handle', handle);
  return token;
}

/** Sign in from any browser: recover the identity key from escrow and
 *  enroll a "Web browser" device (revocable in the app's Settings). */
export async function signIn({ handle, password }) {
  const v = await api('/v1/identity/escrow_fetch', {
    handle,
    path: 'password',
  });
  const identitySeed = await openEscrow(password, unb64(v.blob));
  const deviceSeed = crypto.getRandomValues(new Uint8Array(32));
  const devicePub = await getPublicKeyAsync(deviceSeed);
  const created = Math.floor(Date.now() / 1000);
  const name = 'Web browser';
  await api('/v1/identity/recover_device', {
    account_id: v.account_id,
    device_pub: b64(devicePub),
    device_name: name,
    device_created: created,
    device_cert: await deviceCert(identitySeed, devicePub, name, created),
  });
  const token = await authWith(v.account_id, deviceSeed);
  sessionStorage.setItem('hive_token', token);
  sessionStorage.setItem('hive_handle', handle);
  return token;
}

export function currentToken() {
  return sessionStorage.getItem('hive_token');
}

export function currentHandle() {
  return sessionStorage.getItem('hive_handle');
}

/** Fetch the session-gated APK and hand it to the browser as a download. */
export async function downloadApk() {
  const token = currentToken();
  if (!token) throw new Error('not signed in');
  const resp = await fetch('/v1/app/download_ticket', {
    method: 'POST',
    headers: { Authorization: 'Bearer ' + token },
  });
  let v = null;
  try {
    v = await resp.json();
  } catch {
    throw new Error('download failed');
  }
  if (!resp.ok) {
    throw new Error(
      resp.status === 401 ? 'session expired — sign in again' : 'download failed'
    );
  }
  if (!v.ok) throw new Error(v.err || 'download failed');
  if (!v.url) throw new Error('download link missing — try again');
  window.location.assign(v.url);
  return v.size || 0;
}
