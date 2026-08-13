// Owns sign-in form state, validation, errors, and successful-page navigation.
// hive.js owns recovery decryption, device enrollment, authentication, and session storage.

import { signIn } from './hive.js';

const $ = (id) => document.getElementById(id);

$('form').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('err').textContent = '';

  const handle = $('handle').value.trim().replace(/^@/, '');
  const password = $('password').value;
  if (!handle || !password)
    return ($('err').textContent = 'Enter your handle and password.');

  $('go').disabled = true;
  $('status').textContent = 'Unlocking your keys…';
  try {
    await signIn({ handle, password });
    location.href = 'download.html';
  } catch (ex) {
    $('err').textContent = ex.message;
    $('status').textContent = '';
    $('go').disabled = false;
  }
});
