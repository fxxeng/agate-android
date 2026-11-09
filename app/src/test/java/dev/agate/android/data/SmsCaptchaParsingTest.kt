package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * 短信登录「风控挑战」的解析判据。
 *
 * 这些用例钉的是**流程状态**，不是文案：返回 null 表示这一轮真的发出码了
 * （界面进入验证码输入），非 null 表示没发出码（界面停在打开验证页 / 重发）。
 * 判错方向的代价是用户看着一个永远不会来的短信，或者滑块白过。
 */
class SmsCaptchaParsingTest {

    @Test
    fun captcha_response_is_parsed_with_all_three_fields() {
        val data = JSONObject(
            """
            {"status":"captcha","uuid":"u-1","ticket":"u-1",
             "captcha":{"requestCode":"rc-1",
                        "verifyUrl":"https://verify.meituan.com/v2/verify?id=1",
                        "message":"需要进行人机验证"}}
            """.trimIndent()
        )
        assertEquals(
            SmsCaptcha("需要进行人机验证", "https://verify.meituan.com/v2/verify?id=1", "rc-1"),
            AccountOps.smsCaptcha(data),
        )
    }

    @Test
    fun normal_send_response_is_not_a_challenge() {
        assertNull(AccountOps.smsCaptcha(null))
        assertNull(AccountOps.smsCaptcha(JSONObject("""{"uuid":"u-1","ticket":"u-1"}""")))
        assertNull(AccountOps.smsCaptcha(JSONObject("""{"status":"ok"}""")))
    }

    @Test
    fun captcha_without_usable_verify_url_is_not_shown() {
        // 没有可打开的页面就不该显示「打开验证页」按钮，否则只会给用户一个死路
        assertNull(
            AccountOps.smsCaptcha(
                JSONObject("""{"status":"captcha","captcha":{"message":"需要验证"}}""")
            )
        )
        assertNull(AccountOps.smsCaptcha(JSONObject("""{"status":"captcha"}""")))
    }

    @Test
    fun missing_or_null_message_falls_back_to_readable_text() {
        // ⚠️ 真机 libcore 的 optString 把 JSONObject.NULL 读成字面量 "null"
        // （单测用的 org.json 返回 default，测不出这个差异）—— 用已被污染的
        // 字符串形状钉住：界面绝不能把 "null" 当成原因显示给用户。
        val literal = JSONObject(
            """{"status":"captcha","captcha":{"verifyUrl":"https://verify.meituan.com/x","message":"null"}}"""
        )
        assertEquals("需要进行人机验证", AccountOps.smsCaptcha(literal)?.message)

        val missing = JSONObject(
            """{"status":"captcha","captcha":{"verifyUrl":"https://verify.meituan.com/x"}}"""
        )
        assertEquals("需要进行人机验证", AccountOps.smsCaptcha(missing)?.message)
        assertEquals("", AccountOps.smsCaptcha(missing)?.requestCode)
    }

    @Test
    fun blank_verify_url_is_rejected() {
        val data = JSONObject(
            """{"status":"captcha","captcha":{"verifyUrl":"   ","message":"需要验证"}}"""
        )
        assertNull(AccountOps.smsCaptcha(data))
    }
}
