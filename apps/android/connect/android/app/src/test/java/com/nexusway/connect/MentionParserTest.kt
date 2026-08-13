package com.nexusway.connect

import org.junit.Assert.assertEquals
import org.junit.Test

class MentionParserTest {
    @Test
    fun parsesLegacyAndSpacedHandlesWithLongestMatch() {
        val body = "Thanks @Jane Doe and @jane."

        assertEquals(
            listOf(
                MentionMatch(7, 16, "Jane Doe"),
                MentionMatch(21, 26, "jane"),
            ),
            findMentionMatches(body, listOf("Jane", "Jane Doe", "jane")),
        )
    }

    @Test
    fun quotedMentionSupportsUnknownSpacedHandle() {
        val body = "Hello @\"New Person\" today"

        assertEquals(
            listOf(MentionMatch(6, 19, "New Person")),
            findMentionMatches(body, emptyList()),
        )
    }

    @Test
    fun knownHandleRequiresBoundary() {
        assertEquals(
            listOf(MentionMatch(6, 11, "Jane")),
            findMentionMatches("Hello @Jane DoeMore", listOf("Jane Doe")),
        )
    }
}