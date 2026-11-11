package dev.agate.android.auth

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AuthCallbackPolicyTest {
    private val state = "one-time-state"
    private val callback = "office-raccoon://auth/callback?state=$state&code=abc"

    @Test
    fun exactRaccoonCallbackIsAllowed() {
        assertTrue(AuthCallbackPolicy.shouldForward("raccoon", state, callback))
    }

    @Test
    fun otherProvidersNeverGainDeepLinkAccess() {
        for (provider in listOf("antigravity", "accio", "catpaw", "")) {
            assertFalse(AuthCallbackPolicy.shouldForward(provider, state, callback))
        }
        assertFalse(AuthCallbackPolicy.shouldForward("raccoon", "", callback))
    }

    @Test
    fun spoofedOrMalformedCallbackIsRejected() {
        val bad = listOf(
            "office-raccoon://auth.evil/callback?state=$state",
            "office-raccoon://auth/callback.evil?state=$state",
            "office-raccoon://auth@evil/callback?state=$state",
            "office-raccoon://auth:443/callback?state=$state",
            "office-raccoon://auth/callback?state=$state#fragment",
            "office-raccoon://auth/callback?state=$state&state=$state",
            "office-raccoon://auth/callback?state=wrong",
            "office-raccoon://auth/callback?state",
            "office-raccoon://auth/callback?state=%ZZ",
            "OFFICE-RACCOON://auth/callback?state=$state",
            "https://auth/callback?state=$state",
        )
        bad.forEach { url ->
            assertFalse(url, AuthCallbackPolicy.shouldForward("raccoon", state, url))
        }
    }
}
