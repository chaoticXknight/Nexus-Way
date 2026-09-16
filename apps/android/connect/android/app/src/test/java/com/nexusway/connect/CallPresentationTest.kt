package com.nexusway.connect

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test

class CallPresentationTest { // Keep caller identification and invite lifetime independent of Android runtime services.
    @Test fun prefersMatchingProfileName() { // Display the authenticated sender's profile name.
        val profile = JSONObject().put("account_id", "alice").put("display_name", " Alice ").put("handle", "alice123")
        assertEquals("Alice", incomingCallerLabel("alice", profile, "old-handle"))
    }

    @Test fun rejectsAnotherAccountsProfile() { // A mismatched lookup must not impersonate another contact.
        val profile = JSONObject().put("account_id", "bob").put("display_name", "Bob")
        assertEquals("@alice123", incomingCallerLabel("alice", profile, "alice123"))
    }

    @Test fun fallsBackToProfileThenKnownHandle() { // Missing display names and unavailable lookups still identify known callers.
        val profile = JSONObject().put("account_id", "alice").put("display_name", " ").put("handle", "alice123")
        assertEquals("@alice123", incomingCallerLabel("alice", profile, "old-handle"))
        assertEquals("@alice123", incomingCallerLabel("alice", null, "@alice123"))
        assertEquals("Connect contact 12345678", incomingCallerLabel("1234567890", null, ""))
    }

    @Test fun expiredInviteCannotRingAgain() { // Stale notifications must not become answerable after redelivery.
        assertEquals(0L, incomingCallRemainingMillis(99, 100_000))
        assertEquals(0L, incomingCallRemainingMillis(100, 100_000))
    }

    @Test fun retriesPreserveRemainingRingTime() { // A nearly expired call gets only its remaining ring window.
        assertEquals(5_000L, incomingCallRemainingMillis(105, 100_000))
        assertEquals(60_000L, incomingCallRemainingMillis(Long.MAX_VALUE, 100_000))
    }
}