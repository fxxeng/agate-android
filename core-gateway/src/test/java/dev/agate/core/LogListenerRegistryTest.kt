package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

class LogListenerRegistryTest {
    @Test
    fun parsesSuccessfulRegistrationAndClearResponses() {
        val registered = parseLogCallbackUpdateResponse("""{"ok":true}""")
        val cleared = parseLogCallbackUpdateResponse("""{"ok":true,"cleared":true}""")

        assertEquals(true, registered.ok)
        assertEquals(false, registered.cleared)
        assertEquals(true, cleared.ok)
        assertEquals(true, cleared.cleared)
    }

    @Test
    fun parsesNativeFailureWithoutTreatingItAsSuccess() {
        val result = parseLogCallbackUpdateResponse(
            """{"ok":false,"error":"log_callback_lock_poisoned"}""",
        )

        assertEquals(false, result.ok)
        assertEquals("log_callback_lock_poisoned", result.error)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsResponseWithoutOkField() {
        parseLogCallbackUpdateResponse("""{"error":"missing_ok"}""")
    }

    @Test
    fun successfulRegistrationReplacesCurrentListener() {
        val registry = LogListenerRegistry<Any>()
        val first = Any()
        val second = Any()

        val firstResult = registry.update(first) { LogCallbackUpdateResult(ok = true) }
        val secondResult = registry.update(second) { LogCallbackUpdateResult(ok = true) }

        assertEquals(true, firstResult.ok)
        assertEquals(true, secondResult.ok)
        assertSame(second, registry.current())
    }

    @Test
    fun failedRegistrationPreservesPreviousListener() {
        val registry = LogListenerRegistry<Any>()
        val first = Any()
        val rejected = Any()
        registry.update(first) { LogCallbackUpdateResult(ok = true) }

        val result = registry.update(rejected) {
            LogCallbackUpdateResult(ok = false, error = "global_ref_failed")
        }

        assertEquals(false, result.ok)
        assertEquals("global_ref_failed", result.error)
        assertSame(first, registry.current())
    }

    @Test
    fun failedClearPreservesPreviousListener() {
        val registry = LogListenerRegistry<Any>()
        val first = Any()
        registry.update(first) { LogCallbackUpdateResult(ok = true) }

        val result = registry.update(null) {
            LogCallbackUpdateResult(ok = false, error = "callback_lock_poisoned")
        }

        assertEquals(false, result.ok)
        assertSame(first, registry.current())
    }

    @Test
    fun successfulClearDropsPreviousListener() {
        val registry = LogListenerRegistry<Any>()
        registry.update(Any()) { LogCallbackUpdateResult(ok = true) }

        val result = registry.update(null) {
            LogCallbackUpdateResult(ok = true, cleared = true)
        }

        assertEquals(true, result.ok)
        assertEquals(true, result.cleared)
        assertNull(registry.current())
    }

    @Test
    fun nativeExceptionDoesNotReplacePreviousListener() {
        val registry = LogListenerRegistry<Any>()
        val first = Any()
        registry.update(first) { LogCallbackUpdateResult(ok = true) }

        try {
            registry.update(Any()) { throw IllegalStateException("jni unavailable") }
            throw AssertionError("Expected Native exception")
        } catch (expected: IllegalStateException) {
            assertEquals("jni unavailable", expected.message)
        }

        assertSame(first, registry.current())
    }
}
