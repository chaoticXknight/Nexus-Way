package com.nexusway.connect

import org.json.JSONObject

fun incomingCallerLabel(accountId: String, profile: JSONObject?, knownHandle: String): String { // Never use unsigned display-name fields from a signaling frame.
    val verifiedProfile = profile?.takeIf { it.optString("account_id") == accountId } // A lookup must describe the authenticated sender.
    return verifiedProfile?.optString("display_name")?.trim()?.takeIf { it.isNotEmpty() } // Prefer the caller's chosen profile name.
        ?: (verifiedProfile?.optString("handle")?.trim()?.takeIf { it.isNotEmpty() } // Fall back to the profile handle.
            ?: knownHandle.trim().takeIf { it.isNotEmpty() })?.let { "@${it.removePrefix("@")}" } // Verified message history also identifies a known contact offline.
        ?: "Connect contact ${accountId.take(8)}" // Keep an unknown caller distinguishable without inventing a name.
}

fun incomingCallRemainingMillis(expiresAtSeconds: Long, nowMillis: Long): Long { // Notification retries must not extend the original ringing window.
    if (expiresAtSeconds <= nowMillis / 1_000) return 0 // Expired calls have no actionable notification.
    return (expiresAtSeconds - nowMillis / 1_000).coerceAtMost(60) * 1_000 // Limit both legacy and server-provided expiry values to one minute.
}