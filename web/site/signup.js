// Owns invite-link handling, signup validation, and signup-page navigation.
// hive.js owns key generation, escrow encryption, registration, and session storage.

import { signUp } from './hive.js?v=20260719-invitelink';

const $ = (id) => document.getElementById(id);

const params = new URLSearchParams(location.search);
const inviteFromLink = params.get('invite') || '';
if (inviteFromLink) {
  $('invite').value = inviteFromLink;
  $('invite').readOnly = true;
  $('invite-status').textContent = 'Invite link applied.';
}

$('form').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('err').textContent = '';

  const invite = $('invite').value.trim();
  const handle = $('handle').value.trim().replace(/^@/, '');
  const password = $('password').value;

  if (!handle) return ($('err').textContent = 'Pick a handle.');
  if (password.length < 8)
    return ($('err').textContent = 'Password must be at least 8 characters.');
  if (password !== $('confirm').value)
    return ($('err').textContent = 'Passwords do not match.');
  if (!$('age').checked)
    return ($('err').textContent = 'You must confirm you are 18 or older.');
  if (!$('agree').checked)
    return ($('err').textContent = 'You must agree to the Terms and Privacy Policy.');

  $('go').disabled = true;
  $('status').textContent = 'Generating your keys…';
  try {
    await signUp({ invite, handle, password });
    $('status').textContent = 'Account created!';
    location.href = 'download.html';
  } catch (ex) {
    $('err').textContent = ex.message;
    $('status').textContent = '';
    $('go').disabled = false;
  }
});
