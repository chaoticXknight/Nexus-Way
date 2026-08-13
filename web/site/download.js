// Owns download-page session gating, user feedback, and browser navigation.
// hive.js exchanges the session for a one-use URL; HIVE serves the APK bytes.

import { currentToken, currentHandle, downloadApk } from './hive.js?v=20260719-ticketdownload';

const $ = (id) => document.getElementById(id);

// Gate: no session → go sign in.
if (!currentToken()) {
  location.replace('login.html');
} else {
  const h = currentHandle();
  if (h) $('hello').textContent = `You're in, @${h}.`;
}

$('dl').addEventListener('click', async () => {
  $('err').textContent = '';
  $('dl').disabled = true;
  $('status').textContent = 'Downloading…';
  try {
    const bytes = await downloadApk();
    const mb = (bytes / (1024 * 1024)).toFixed(1);
    $('status').textContent = `Download started (${mb} MB) — check your browser downloads.`;
  } catch (ex) {
    $('err').textContent = ex.message;
    $('status').textContent = '';
    if (/sign in/.test(ex.message)) {
      sessionStorage.removeItem('hive_token');
      setTimeout(() => location.replace('login.html'), 1200);
    }
  }
  $('dl').disabled = false;
});
