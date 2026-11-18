package dev.agate.android.ui.provider

import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 阿里云滑块宿主页的契约。
 *
 * 这里不测 SDK 本身（那是浏览器里跑的），只钉住**我们负责的那一半**：
 * `prefix` / `sceneId` 必须按 JSON 字面量嵌进脚本 —— 上游哪天在 sceneId 里带
 * 引号或反斜杠，字符串拼接会让整段 JS 语法错误，表现出来就是「滑块一直不出现」
 * （与本次要修的故障同一个症状，但根因在转义）。
 */
class AutoClawCaptchaPageTest {

    @Test
    fun emits_sdk_config_callback_and_quoted_values() {
        val html = captchaPage(
            JSONObject.quote("ga"),
            JSONObject.quote("issuer-prefix"),
            JSONObject.quote("scene-123"),
        )
        assertTrue(html.contains("https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js"))
        assertTrue(html.contains("window.initAliyunCaptcha"))
        assertTrue(html.contains("captchaVerifyCallback"))
        assertTrue(html.contains("AgateCaptcha.onParam"))
        assertTrue(html.contains("\"scene-123\""))
    }

    @Test
    fun quotes_special_characters_instead_of_breaking_the_script() {
        val html = captchaPage(
            JSONObject.quote("ga"),
            JSONObject.quote("p\"ref\\ix"),
            JSONObject.quote("scene\"\\1"),
        )
        // 引号与反斜杠被转义成合法 JSON 字符串，而不是裸露在 JS 源码里
        assertTrue(html.contains("\\\""))
        assertTrue(html.contains("\\\\"))
        assertFalse(html.contains("SceneId: scene"))
    }
}
